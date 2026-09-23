// Copyright 2025 PackDB Contributors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! `packdb-query` — command-line SQL client for PackDB.
//!
//! Sends a SQL query to a PackDB server and prints the result as a table,
//! JSON or CSV. Built with the `flight-sql` feature it prefers Flight SQL and
//! falls back to HTTP; `--http` forces HTTP.
//!
//! The flags and environment variables below are the supported CLI contract;
//! see the repository's compatibility documentation before changing one.

// Production-grade linting: Deny all safety-critical issues
#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]
#![deny(clippy::indexing_slicing)]
#![deny(clippy::panic)]
#![deny(clippy::unreachable)]

use clap::Parser;
use packdb_client::{
    AuthConfig, ClientOutcome, ClientResult, OutputFormat, SqlClient, SqlClientConfig,
};
use std::process::exit;

#[derive(Parser)]
#[command(name = "packdb-query")]
#[command(about = "PackDB Query CLI - Execute SQL queries against PackDB server")]
#[command(version)]
#[command(long_about = r#"Execute SQL queries against a running PackDB server.

Exit Codes:
  0 - Success
  1 - SQL error (query parsing or execution failure)
  2 - Invalid arguments, configuration, connection or I/O failure
  3 - Protocol error (invalid response from server)

Exit code 2 covers both a bad invocation and an unreachable server. To tell
them apart, read stderr: argument errors begin "error:", everything else
begins "ERROR:".

Examples:
  # Basic query
  packdb-query --url http://localhost:4000 "SELECT * FROM metrics.cpu_usage"

  # With authentication
  packdb-query --url http://localhost:4000 --api-key YOUR_KEY "SELECT COUNT(*) FROM metrics"

  # Using environment variables
  export PACKDB_URL=http://localhost:4000
  export PACKDB_API_KEY=your-api-key
  packdb-query "SELECT * FROM logs LIMIT 10"

  # Different output formats
  packdb-query --format json "SELECT * FROM traces"
  packdb-query --format csv "SELECT timestamp, value FROM metrics"
"#)]
struct Cli {
    /// PackDB server URL
    #[arg(long, env = "PACKDB_URL", default_value = "http://localhost:4000")]
    url: String,

    /// API key for authentication
    #[arg(long, env = "PACKDB_API_KEY")]
    api_key: Option<String>,

    /// Bearer token for authentication
    #[arg(long, env = "PACKDB_TOKEN")]
    token: Option<String>,

    /// Basic authentication (format: user:password)
    #[arg(long)]
    basic_auth: Option<String>,

    /// Request timeout in seconds
    #[arg(long, default_value = "30")]
    timeout: u64,

    /// Output format
    #[arg(long, default_value = "table", value_parser = ["table", "json", "csv"])]
    format: String,

    /// Flight SQL endpoint (gRPC URL, e.g., http://localhost:4320)
    ///
    /// Only available when compiled with the `flight-sql` feature.
    #[cfg(feature = "flight-sql")]
    #[arg(long, env = "PACKDB_FLIGHT_URL")]
    flight_endpoint: Option<String>,

    /// Force HTTP protocol (skip Flight SQL)
    ///
    /// Only available when compiled with the `flight-sql` feature.
    #[cfg(feature = "flight-sql")]
    #[arg(long)]
    http_only: bool,

    /// SQL query to execute
    sql: String,
}

/// Turn parsed arguments into the client configuration and output format.
///
/// Split out of `main` so it can be asserted on. `main` is `#[tokio::main]`
/// and leaves through `exit`, so nothing decided inline there is reachable
/// from a test — which is why this file measured 0 covered lines. Every
/// decision about what the client will talk to lives here: output format,
/// authentication precedence, and the `flight-sql` transport selection.
///
/// Errors carry their own exit code, so `main` stays a thin wrapper that
/// reports and exits.
fn build_client_config(cli: Cli) -> ClientResult<(SqlClientConfig, OutputFormat)> {
    let format = OutputFormat::parse(&cli.format)?;
    let auth = AuthConfig::from_cli_options(cli.api_key, cli.token, cli.basic_auth)?;

    // `SqlClientConfig::auth` is `Arc<dyn AuthProvider>` (same seam as
    // PromQL/LogQL), so we adapt the static `AuthConfig` via
    // `into_provider()`. Behavior is unchanged for CLI users —
    // `AuthConfigProvider` wraps the existing `apply_to_headers` logic.
    //
    // Without the `flight-sql` feature the CLI has no way to select the
    // gRPC transport, so the config is pinned to HTTP.
    let config = SqlClientConfig {
        url: cli.url,
        #[cfg(feature = "flight-sql")]
        flight_url: cli.flight_endpoint,
        #[cfg(not(feature = "flight-sql"))]
        flight_url: None,
        auth: auth.into_provider(),
        timeout: std::time::Duration::from_secs(cli.timeout),
        #[cfg(feature = "flight-sql")]
        http_only: cli.http_only,
        #[cfg(not(feature = "flight-sql"))]
        http_only: true,
    };

    Ok((config, format))
}

async fn run_query(cli: Cli) -> ClientOutcome {
    let sql = cli.sql.clone();

    match build_client_config(cli) {
        Ok((config, format)) => {
            ClientOutcome::from_result(SqlClient::new(config).execute_query(&sql, format).await)
        }
        Err(error) => ClientOutcome::from_result(Err(error)),
    }
}

#[tokio::main]
async fn main() {
    let outcome = run_query(Cli::parse()).await;

    print!("{}", outcome.stdout());
    eprint!("{}", outcome.stderr());
    exit(outcome.exit_code());
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    /// Parse a full argv the way the binary receives it, so the test
    /// exercises the real clap surface rather than a hand-built `Cli`.
    fn cli_from(args: &[&str]) -> Cli {
        Cli::try_parse_from(args).expect("arguments should parse")
    }

    #[test]
    fn defaults_produce_an_http_config_with_the_documented_timeout() {
        let cli = cli_from(&["packdb-query", "SELECT 1"]);
        let (config, format) = build_client_config(cli).expect("defaults should build");

        assert_eq!(config.url, "http://localhost:4000");
        assert_eq!(config.timeout, std::time::Duration::from_secs(30));
        assert!(
            matches!(format, OutputFormat::Table),
            "the documented default format is table"
        );
    }

    #[test]
    fn each_documented_output_format_is_accepted() {
        for (arg, expected) in [
            ("table", OutputFormat::Table),
            ("json", OutputFormat::Json),
            ("csv", OutputFormat::Csv),
        ] {
            let cli = cli_from(&["packdb-query", "--format", arg, "SELECT 1"]);
            let (_, format) = build_client_config(cli).expect("documented format should build");
            assert_eq!(
                std::mem::discriminant(&format),
                std::mem::discriminant(&expected),
                "--format {arg} should select {expected:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_malformed_basic_auth_pair_fails_before_query_execution() {
        // clap accepts the string; the rejection is ours, and the CLI's
        // documented exit code for a config problem is 2. Running the full
        // query seam also proves this failure does not reach a transport.
        let cli = cli_from(&["packdb-query", "--basic-auth", "no-colon", "SELECT 1"]);
        let outcome = run_query(cli).await;

        assert_eq!(outcome.stdout(), "");
        assert_eq!(
            outcome.stderr(),
            "ERROR: Configuration error: Basic auth must be in format 'user:password'\n"
        );
        assert_eq!(
            outcome.exit_code(),
            2,
            "config errors exit 2 per the CLI contract"
        );
    }

    #[test]
    fn api_key_takes_precedence_over_other_credentials() {
        // Precedence is api_key > token > basic_auth. A malformed basic-auth
        // pair alongside an api key must NOT fail: the api key wins before
        // the pair is ever parsed. This pins the ordering, not just success.
        let cli = cli_from(&[
            "packdb-query",
            "--api-key",
            "k",
            "--basic-auth",
            "no-colon",
            "SELECT 1",
        ]);
        assert!(
            build_client_config(cli).is_ok(),
            "api_key should be selected before basic_auth is parsed"
        );
    }

    #[cfg(feature = "flight-sql")]
    #[test]
    fn flight_endpoint_reaches_the_client_config() {
        let cli = cli_from(&[
            "packdb-query",
            "--flight-endpoint",
            "http://localhost:4320",
            "SELECT 1",
        ]);
        let (config, _) = build_client_config(cli).expect("flight endpoint should build");

        assert_eq!(config.flight_url.as_deref(), Some("http://localhost:4320"));
        assert!(
            !config.http_only,
            "an explicit Flight SQL endpoint must not be pinned to HTTP"
        );
    }

    #[cfg(feature = "flight-sql")]
    #[test]
    fn http_only_wins_even_when_a_flight_endpoint_is_supplied() {
        // The flag exists to force HTTP, so it has to beat a configured
        // endpoint rather than being ignored when both are present.
        let cli = cli_from(&[
            "packdb-query",
            "--flight-endpoint",
            "http://localhost:4320",
            "--http-only",
            "SELECT 1",
        ]);
        let (config, _) = build_client_config(cli).expect("http-only should build");

        assert!(
            config.http_only,
            "--http-only must force the HTTP transport"
        );
    }

    #[cfg(not(feature = "flight-sql"))]
    #[test]
    fn without_the_feature_the_config_is_pinned_to_http() {
        let cli = cli_from(&["packdb-query", "SELECT 1"]);
        let (config, _) = build_client_config(cli).expect("defaults should build");

        assert!(config.flight_url.is_none());
        assert!(config.http_only);
    }
}
