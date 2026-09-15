# azmon-report

A Rust CLI that runs both Azure Log Analytics workspace queries and Application Insights application queries, then generates Markdown, CSV, and JSON reports.

## Features

- Multiple KQL queries in one YAML configuration
- Log Analytics and Application Insights data endpoints
- Authentication through the existing Azure CLI session
- Optional `AZURE_MONITOR_TOKEN` environment variable for automation
- Per-query ISO 8601 timespans
- Markdown summary plus CSV and JSON artifacts
- Windows 11 and Git Bash friendly

## Prerequisites

- Rust toolchain (`rustup` and `cargo`)
- Azure CLI
- Access to the target workspace or Application Insights application

```bash
az login
az account show
```

## Build

```bash
cp config.example.yaml config.yaml
cargo build --release
```

## Configure

Edit `config.yaml`. For `log_analytics`, use the Log Analytics workspace ID. For `application_insights`, use the Application ID from the Application Insights API Access page.

A query can be embedded with `query:` or loaded from a `.kql` file with `query_file:`. Exactly one is required.

## Validate and run

```bash
cargo run -- validate --config config.yaml
cargo run -- run --config config.yaml
```

Or run the release executable:

```bash
./target/release/azmon-report.exe run --config config.yaml
```

Generated files are written to the configured output directory:

```text
reports/
  report.md
  01-filesender-failures.csv
  01-filesender-failures.json
  02-application-request-summary.csv
```

## Non-interactive token option

If your pipeline already obtains the appropriate data-plane bearer token, provide it directly:

```bash
export AZURE_MONITOR_TOKEN="$(az account get-access-token --resource https://api.loganalytics.io --query accessToken -o tsv)"
cargo run -- run --config config.yaml
```

A single supplied token should only be used when every configured target accepts that token. Without this variable, the tool automatically requests and caches separate Azure CLI tokens for the Log Analytics and Application Insights resources.

## Suggested next enhancements

- Service-principal and managed-identity authentication
- HTML report templates
- Thresholds and nonzero exit codes for failed health checks
- Parallel query execution
- Charts generated from numeric result columns
- OpenTelemetry instrumentation for the CLI itself
