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

//! `packdb query sql` through [`packdb_cli::run`] against a mock server.

#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::collections::HashMap;
use std::path::Path;

mod support;

use packdb_cli::{
    Command, ConnectionArgs, Environment, Exit, Io, QueryCommand, QueryFormat, SqlArgs,
    TransportChoice, run,
};
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Default)]
struct Env(HashMap<String, String>);

impl Env {
    fn with(mut self, name: &str, value: &str) -> Self {
        self.0.insert(name.into(), value.into());
        self
    }
}

impl Environment for Env {
    fn var(&self, name: &str) -> Option<String> {
        self.0.get(name).cloned()
    }

    fn read_file(&self, _: &Path, _: u64) -> std::io::Result<Vec<u8>> {
        Err(std::io::Error::from(std::io::ErrorKind::NotFound))
    }
}

struct Outcome {
    exit: Exit,
    stdout: String,
    stderr: String,
}

async fn sql(endpoint: &str, format: QueryFormat, env: Env) -> Outcome {
    let command = Command::Query {
        language: QueryCommand::Sql(SqlArgs {
            sql: "SELECT count".into(),
            connection: ConnectionArgs {
                endpoint: Some(endpoint.into()),
                ..ConnectionArgs::default()
            },
            transport: TransportChoice::Http,
            format,
        }),
    };
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let exit = run(
        command,
        Io {
            stdout: &mut stdout,
            stderr: &mut stderr,
            env: &env,
        },
    )
    .await;
    Outcome {
        exit,
        stdout: String::from_utf8(stdout).expect("utf-8"),
        stderr: String::from_utf8(stderr).expect("utf-8"),
    }
}

async fn server(response: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/sql"))
        .respond_with(response)
        .mount(&server)
        .await;
    server
}

fn rows() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"rows": [{"count": 7}]}))
}

#[tokio::test]
async fn an_answer_without_a_carrier_prints_rows_and_reports_unknown_completeness() {
    let server = server(rows()).await;
    let outcome = sql(&server.uri(), QueryFormat::Csv, Env::default()).await;
    assert_eq!(outcome.exit, Exit::Incomplete);
    assert!(outcome.stdout.contains("count"), "{}", outcome.stdout);
    assert!(outcome.stdout.contains('7'), "{}", outcome.stdout);
    assert!(
        outcome.stderr.contains("warning: completeness unknown"),
        "{}",
        outcome.stderr
    );
}

#[tokio::test]
async fn partial_causes_reach_json_and_stderr_and_exit_3() {
    let server =
        server(rows().insert_header("x-packdb-partial-causes", "cold_tier_unavailable")).await;
    let outcome = sql(&server.uri(), QueryFormat::Json, Env::default()).await;
    assert_eq!(outcome.exit, Exit::Incomplete);
    let envelope: Value = serde_json::from_str(&outcome.stdout).expect("one JSON object");
    assert_eq!(envelope["schema_version"], packdb_cli::SCHEMA_VERSION);
    assert_eq!(envelope["command"], "query sql");
    assert_eq!(envelope["completeness"], "partial");
    assert_eq!(envelope["partial_causes"], json!(["cold_tier_unavailable"]));
    assert_eq!(envelope["transport"]["requested"], "http");
    assert_eq!(envelope["transport"]["used"], "http");
    assert_eq!(envelope["data"]["rows"], json!([{"count": 7}]));
    assert_eq!(envelope["error"], Value::Null);
    assert!(
        outcome
            .stderr
            .contains("warning: incomplete result (cold_tier_unavailable)"),
        "{}",
        outcome.stderr
    );
}

#[tokio::test]
async fn a_table_never_hides_partial_causes() {
    let server = server(rows().insert_header("x-packdb-partial-causes", "tier_scan_failed")).await;
    let outcome = sql(&server.uri(), QueryFormat::Table, Env::default()).await;
    assert_eq!(outcome.exit, Exit::Incomplete);
    assert!(outcome.stdout.contains("count"));
    assert!(outcome.stderr.contains("tier_scan_failed"));
}

#[tokio::test]
async fn failures_map_to_their_exit_codes_and_print_nothing_on_stdout() {
    let cases = [
        (400, "Query error: bad SQL", Exit::Rejected),
        (401, "missing credentials", Exit::Unauthenticated),
        (403, "not permitted", Exit::PermissionDenied),
        (500, "Storage error: disk full", Exit::ServerFailure),
    ];
    for (status, body, expected) in cases {
        let server = server(ResponseTemplate::new(status).set_body_string(body)).await;
        let outcome = sql(&server.uri(), QueryFormat::Table, Env::default()).await;
        assert_eq!(outcome.exit, expected, "{status}");
        assert_eq!(outcome.stdout, "", "{status}");
        assert!(outcome.stderr.starts_with("error: "), "{}", outcome.stderr);
        assert!(outcome.stderr.contains(body), "{}", outcome.stderr);
    }
}

#[tokio::test]
async fn a_json_failure_prints_the_envelope_with_its_error() {
    let server = server(ResponseTemplate::new(403).set_body_string("not permitted")).await;
    let outcome = sql(&server.uri(), QueryFormat::Json, Env::default()).await;
    assert_eq!(outcome.exit, Exit::PermissionDenied);
    let envelope: Value = serde_json::from_str(&outcome.stdout).expect("one JSON object");
    assert_eq!(envelope["error"]["kind"], "permission_denied");
    assert_eq!(envelope["completeness"], Value::Null);
    assert_eq!(envelope["transport"]["used"], "http");
    assert_eq!(envelope["transport"]["fallback_reason"], Value::Null);
    assert_eq!(envelope["data"], Value::Null);
}

#[tokio::test]
async fn an_unreachable_server_exits_6() {
    let refusing = support::refusing_endpoint();
    let endpoint = refusing.endpoint.clone();
    let outcome = sql(&endpoint, QueryFormat::Table, Env::default()).await;
    assert_eq!(outcome.exit, Exit::Unreachable);
    assert_eq!(outcome.stdout, "");
}

#[tokio::test]
async fn credentials_and_tenant_are_sent_and_never_printed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/sql"))
        .and(header("x-api-key", "secret-api-key"))
        .and(header("x-scope-orgid", "tenant-a"))
        .respond_with(rows())
        .expect(1)
        .mount(&server)
        .await;
    let env = Env::default()
        .with("PACKDB_API_KEY", "secret-api-key")
        .with("PACKDB_TENANT", "tenant-a");
    let outcome = sql(&server.uri(), QueryFormat::Json, env).await;
    assert_eq!(outcome.exit, Exit::Incomplete);
    assert!(!outcome.stdout.contains("secret-api-key"));
    assert!(!outcome.stderr.contains("secret-api-key"));
}

#[tokio::test]
async fn conflicting_credentials_send_nothing_and_exit_2() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(rows())
        .expect(0)
        .mount(&server)
        .await;
    let env = Env::default()
        .with("PACKDB_API_KEY", "a")
        .with("PACKDB_TOKEN", "b");
    let outcome = sql(&server.uri(), QueryFormat::Table, env).await;
    assert_eq!(outcome.exit, Exit::Usage);
    assert_eq!(outcome.stdout, "");
    assert!(outcome.stderr.contains("conflicting credentials"));
}

#[tokio::test]
async fn an_invalid_tenant_is_refused_before_sending() {
    let env = Env::default().with("PACKDB_TENANT", "bad\ntenant");
    let outcome = sql("http://127.0.0.1:9", QueryFormat::Table, env).await;
    assert_eq!(outcome.exit, Exit::Usage);
    assert!(outcome.stderr.contains("tenant"));
}

#[tokio::test]
async fn explicit_flight_sql_without_an_endpoint_is_a_usage_error() {
    let command = Command::Query {
        language: QueryCommand::Sql(SqlArgs {
            sql: "SELECT 1".into(),
            connection: ConnectionArgs::default(),
            transport: TransportChoice::FlightSql,
            format: QueryFormat::Table,
        }),
    };
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let exit = run(
        command,
        Io {
            stdout: &mut stdout,
            stderr: &mut stderr,
            env: &Env::default(),
        },
    )
    .await;
    assert_eq!(exit, Exit::Usage);
    assert!(stdout.is_empty());
}

/// A writer that fails like a full disk or a closed pipe.
struct FailingWriter;

impl std::io::Write for FailingWriter {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "the reader went away",
        ))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn sql_command(endpoint: &str) -> Command {
    Command::Query {
        language: QueryCommand::Sql(SqlArgs {
            sql: "SELECT count".into(),
            connection: ConnectionArgs {
                endpoint: Some(endpoint.into()),
                ..ConnectionArgs::default()
            },
            transport: TransportChoice::Http,
            format: QueryFormat::Csv,
        }),
    }
}

#[tokio::test]
async fn a_result_that_cannot_be_written_to_stdout_exits_8_and_says_so() {
    let server = server(rows()).await;
    let mut stderr = Vec::new();
    let exit = run(
        sql_command(&server.uri()),
        Io {
            stdout: &mut FailingWriter,
            stderr: &mut stderr,
            env: &Env::default(),
        },
    )
    .await;
    assert_eq!(exit, Exit::OutputFailed);
    let stderr = String::from_utf8(stderr).expect("utf-8");
    assert!(
        stderr.contains("cannot write the result to stdout"),
        "{stderr}"
    );
}

#[tokio::test]
async fn warnings_that_cannot_be_written_exit_8_even_when_stdout_succeeds() {
    let server = server(rows()).await;
    let mut stdout = Vec::new();
    let exit = run(
        sql_command(&server.uri()),
        Io {
            stdout: &mut stdout,
            stderr: &mut FailingWriter,
            env: &Env::default(),
        },
    )
    .await;
    assert_eq!(exit, Exit::OutputFailed);
    assert!(!stdout.is_empty());
}

#[tokio::test]
async fn a_usage_error_that_cannot_be_reported_exits_8() {
    let env = Env::default()
        .with("PACKDB_API_KEY", "a")
        .with("PACKDB_TOKEN", "b");
    let mut stdout = Vec::new();
    let exit = run(
        sql_command("http://127.0.0.1:9"),
        Io {
            stdout: &mut stdout,
            stderr: &mut FailingWriter,
            env: &env,
        },
    )
    .await;
    assert_eq!(exit, Exit::OutputFailed);
}

/// When Flight SQL is refused and HTTP then fails, the failure still reports
/// the transport that failed and why Flight SQL was skipped.
#[cfg(feature = "flight-sql")]
#[tokio::test]
async fn a_failure_after_fallback_reports_the_fallback() {
    let server = server(ResponseTemplate::new(401).set_body_string("who are you")).await;
    let refusing = support::refusing_endpoint();
    let flight = refusing.endpoint.clone();
    let command = Command::Query {
        language: QueryCommand::Sql(SqlArgs {
            sql: "SELECT count".into(),
            connection: ConnectionArgs {
                endpoint: Some(server.uri()),
                flight_endpoint: Some(flight),
                ..ConnectionArgs::default()
            },
            transport: TransportChoice::Auto,
            format: QueryFormat::Json,
        }),
    };
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let exit = run(
        command,
        Io {
            stdout: &mut stdout,
            stderr: &mut stderr,
            env: &Env::default(),
        },
    )
    .await;
    assert_eq!(exit, Exit::Unauthenticated);
    let envelope: Value = serde_json::from_slice(&stdout).expect("one JSON object");
    assert_eq!(envelope["transport"]["requested"], "auto");
    assert_eq!(envelope["transport"]["used"], "http");
    assert!(
        envelope["transport"]["fallback_reason"].is_string(),
        "{envelope}"
    );
    assert_eq!(envelope["error"]["kind"], "unauthenticated");
    let stderr = String::from_utf8(stderr).expect("utf-8");
    assert!(stderr.contains("used HTTP instead"), "{stderr}");
}

#[tokio::test]
async fn csv_columns_keep_the_server_order() {
    let server = server(
        ResponseTemplate::new(200).set_body_json(json!({"rows": [{"zeta": 1, "alpha": 2}]})),
    )
    .await;
    let outcome = sql(&server.uri(), QueryFormat::Csv, Env::default()).await;
    assert_eq!(
        outcome.stdout.lines().next(),
        Some("zeta,alpha"),
        "{}",
        outcome.stdout
    );
}
