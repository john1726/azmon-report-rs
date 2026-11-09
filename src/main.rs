use anyhow::{bail, Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::HashMap, path::{Path, PathBuf}, process::Stdio};
use tokio::process::Command;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run every query in the YAML configuration and generate reports.
    Run {
        #[arg(short, long, default_value = "config.yaml")]
        config: PathBuf,
    },
    /// Validate the configuration without calling Azure.
    Validate {
        #[arg(short, long, default_value = "config.yaml")]
        config: PathBuf,
    },
}

#[derive(Debug, Deserialize)]
struct Config {
    title: String,
    #[serde(default = "default_timespan")]
    timespan: String,
    #[serde(default = "default_output")]
    output_directory: PathBuf,
    authentication: Authentication,
    queries: Vec<QuerySpec>,
}

fn default_timespan() -> String { "PT24H".into() }
fn default_output() -> PathBuf { "reports".into() }

#[derive(Debug, Deserialize)]
struct Authentication { mode: AuthMode }

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AuthMode { AzureCli }

#[derive(Debug, Deserialize)]
struct QuerySpec {
    name: String,
    source: Source,
    query: Option<String>,
    query_file: Option<PathBuf>,
    #[serde(default = "default_formats")]
    formats: Vec<Format>,
    timespan: Option<String>,
}

fn default_formats() -> Vec<Format> { vec![Format::Markdown, Format::Csv, Format::Json] }

#[derive(Debug, Deserialize)]
struct Source { kind: SourceKind, id: String }

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum SourceKind { LogAnalytics, ApplicationInsights }

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum Format { Markdown, Csv, Json }

#[derive(Debug, Serialize, Deserialize)]
struct QueryResponse { tables: Vec<Table> }

#[derive(Debug, Serialize, Deserialize)]
struct Table { name: String, columns: Vec<Column>, rows: Vec<Vec<Value>> }

#[derive(Debug, Serialize, Deserialize)]
struct Column { name: String, #[serde(rename = "type")] data_type: String }

#[derive(Serialize)]
struct QueryBody<'a> { query: &'a str, timespan: &'a str }

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Run { config } => run(&config).await,
        Commands::Validate { config } => {
            let cfg = load_config(&config)?;
            validate(&cfg, config.parent().unwrap_or(Path::new(".")))?;
            println!("Configuration is valid: {} queries", cfg.queries.len());
            Ok(())
        }
    }
}

fn load_config(path: &Path) -> Result<Config> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    serde_yaml::from_str(&text).context("invalid YAML configuration")
}

fn validate(cfg: &Config, base: &Path) -> Result<()> {
    if cfg.queries.is_empty() { bail!("configuration contains no queries"); }
    for q in &cfg.queries {
        if q.source.id.trim().is_empty() { bail!("query '{}' has an empty source id", q.name); }
        match (&q.query, &q.query_file) {
            (Some(_), None) => {},
            (None, Some(p)) if base.join(p).is_file() => {},
            (None, Some(p)) => bail!("query file not found: {}", base.join(p).display()),
            _ => bail!("query '{}' must define exactly one of query or query_file", q.name),
        }
    }
    Ok(())
}

async fn run(config_path: &Path) -> Result<()> {
    let cfg = load_config(config_path)?;
    let base = config_path.parent().unwrap_or(Path::new("."));
    validate(&cfg, base)?;
    std::fs::create_dir_all(&cfg.output_directory)?;

    let client = Client::builder().user_agent("azmon-report/0.1.0").build()?;
    let mut tokens: HashMap<&'static str, String> = HashMap::new();
    let mut markdown_sections = Vec::new();

    for (index, spec) in cfg.queries.iter().enumerate() {
        let query = match (&spec.query, &spec.query_file) {
            (Some(q), None) => q.clone(),
            (None, Some(path)) => std::fs::read_to_string(base.join(path))?,
            _ => unreachable!(),
        };
        let timespan = spec.timespan.as_deref().unwrap_or(&cfg.timespan);
        let resource = match spec.source.kind {
            SourceKind::LogAnalytics => "https://api.loganalytics.io",
            SourceKind::ApplicationInsights => "https://api.applicationinsights.io",
        };
        let token = if let Ok(token) = std::env::var("AZURE_MONITOR_TOKEN") {
            token
        } else if let Some(token) = tokens.get(resource) {
            token.clone()
        } else {
            let token = acquire_cli_token(resource, &cfg.authentication.mode).await?;
            tokens.insert(resource, token.clone());
            token
        };

        println!("Running: {}", spec.name);
        let response = execute_query(&client, spec, &query, timespan, &token).await?;
        let stem = format!("{:02}-{}", index + 1, slugify(&spec.name));
        for format in &spec.formats {
            match format {
                Format::Json => write_json(&cfg.output_directory.join(format!("{stem}.json")), &response)?,
                Format::Csv => write_csv_tables(&cfg.output_directory, &stem, &response)?,
                Format::Markdown => markdown_sections.push(render_markdown(&spec.name, &response)),
            }
        }
    }

    if !markdown_sections.is_empty() {
        let report = format!("# {}\n\nGenerated: {} UTC\n\n{}\n", cfg.title, Utc::now().format("%Y-%m-%d %H:%M:%S"), markdown_sections.join("\n\n"));
        std::fs::write(cfg.output_directory.join("report.md"), report)?;
    }
    println!("Reports written to {}", cfg.output_directory.display());
    Ok(())
}

async fn acquire_cli_token(resource: &str, mode: &AuthMode) -> Result<String> {
    match mode {
        AuthMode::AzureCli => {
            // Windows CreateProcess only auto-resolves ".exe"; az ships as "az.cmd".
            let az_cmd = if cfg!(windows) { "az.cmd" } else { "az" };
            let output = Command::new(az_cmd)
                .args(["account", "get-access-token", "--resource", resource, "--query", "accessToken", "-o", "tsv"])
                .stdin(Stdio::null()).output().await
                .context("failed to run Azure CLI; install it and run `az login`")?;
            if !output.status.success() {
                bail!("Azure CLI authentication failed: {}", String::from_utf8_lossy(&output.stderr).trim());
            }
            let token = String::from_utf8(output.stdout)?.trim().to_owned();
            if token.is_empty() { bail!("Azure CLI returned an empty access token"); }
            Ok(token)
        }
    }
}

async fn execute_query(client: &Client, spec: &QuerySpec, query: &str, timespan: &str, token: &str) -> Result<QueryResponse> {
    let url = match spec.source.kind {
        SourceKind::LogAnalytics => format!("https://api.loganalytics.azure.com/v1/workspaces/{}/query", spec.source.id),
        SourceKind::ApplicationInsights => format!("https://api.applicationinsights.io/v1/apps/{}/query", spec.source.id),
    };
    let response = client.post(url).bearer_auth(token)
        .json(&QueryBody { query, timespan }).send().await?;
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() { bail!("query '{}' failed with {}: {}", spec.name, status, body); }
    serde_json::from_str(&body).with_context(|| format!("invalid query response for '{}'", spec.name))
}

fn write_json(path: &Path, response: &QueryResponse) -> Result<()> {
    std::fs::write(path, serde_json::to_string_pretty(response)?)?;
    Ok(())
}

fn write_csv_tables(directory: &Path, stem: &str, response: &QueryResponse) -> Result<()> {
    for (i, table) in response.tables.iter().enumerate() {
        let suffix = if response.tables.len() == 1 { String::new() } else { format!("-{}-{}", i + 1, slugify(&table.name)) };
        let mut writer = csv::Writer::from_path(directory.join(format!("{stem}{suffix}.csv")))?;
        writer.write_record(table.columns.iter().map(|c| c.name.as_str()))?;
        for row in &table.rows { writer.write_record(row.iter().map(value_to_string))?; }
        writer.flush()?;
    }
    Ok(())
}

fn render_markdown(title: &str, response: &QueryResponse) -> String {
    let mut out = format!("## {}\n", title);
    for table in &response.tables {
        if response.tables.len() > 1 { out.push_str(&format!("\n### {}\n", table.name)); }
        if table.columns.is_empty() { out.push_str("\n_No columns returned._\n"); continue; }
        out.push('\n');
        out.push_str(&format!("| {} |\n", table.columns.iter().map(|c| escape_md(&c.name)).collect::<Vec<_>>().join(" | ")));
        out.push_str(&format!("| {} |\n", table.columns.iter().map(|_| "---").collect::<Vec<_>>().join(" | ")));
        for row in &table.rows {
            out.push_str(&format!("| {} |\n", row.iter().map(|v| escape_md(&value_to_string(v))).collect::<Vec<_>>().join(" | ")));
        }
        out.push_str(&format!("\nRows: {}\n", table.rows.len()));
    }
    out
}

fn value_to_string(value: &Value) -> String {
    match value { Value::Null => String::new(), Value::String(s) => s.clone(), other => other.to_string() }
}
fn escape_md(value: &str) -> String { value.replace('|', "\\|").replace('\n', "<br>") }
fn slugify(value: &str) -> String {
    let slug = value.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect::<String>();
    slug.split('-').filter(|s| !s.is_empty()).collect::<Vec<_>>().join("-")
}
