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

//! `packdb doctor` through [`packdb_cli::run`] against a mock server.

#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

mod support;

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use packdb_cli::{Command, ConnectionArgs, DoctorArgs, Environment, Exit, Io, OperatorFormat, run};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
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

impl Outcome {
    fn envelope(&self) -> Value {
        serde_json::from_str(&self.stdout).expect("one JSON object")
    }

    fn finding(&self, id: &str) -> Value {
        self.envelope()["data"]["findings"]
            .as_array()
            .expect("findings")
            .iter()
            .find(|finding| finding["id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("no finding {id}: {}", self.stdout))
    }
}

async fn doctor(connection: ConnectionArgs, format: OperatorFormat, env: Env) -> Outcome {
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let exit = run(
        Command::Doctor(DoctorArgs { connection, format }),
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

fn at(endpoint: &str) -> ConnectionArgs {
    ConnectionArgs {
        endpoint: Some(endpoint.into()),
        ..ConnectionArgs::default()
    }
}

/// A server whose `/info` answers; every other route fails the test if hit.
async fn server(info: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/info"))
        .respond_with(info)
        .mount(&server)
        .await;
    server
}

fn info() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "name": "packdb", "version": "0.9.0", "build_time": "unknown",
        "start_time": "2026-10-24T00:00:00Z", "uptime_seconds": 12, "environment": "staging"
    }))
}

/// Only `GET /info` reached the server.
async fn assert_only_info(server: &MockServer) {
    for request in server.received_requests().await.expect("recorded") {
        assert_eq!(request.method.as_str(), "GET");
        assert_eq!(request.url.path(), "/info");
    }
}

#[tokio::test]
async fn limited_diagnostics_read_only_info_and_stay_incomplete() {
    let server = server(info()).await;
    let env = Env::default()
        .with("PACKDB_API_KEY", "secret-key")
        .with("PACKDB_TENANT", "tenant-a");
    let outcome = doctor(at(&server.uri()), OperatorFormat::Json, env).await;
    assert_eq!(outcome.exit, Exit::Incomplete, "{}", outcome.stderr);
    let envelope = outcome.envelope();
    assert_eq!(envelope["command"], "doctor");
    assert_eq!(envelope["completeness"], "unknown");
    assert_eq!(envelope["error"], Value::Null);
    let report = &envelope["data"];
    assert_eq!(report["schema_version"], packdb_cli::REPORT_SCHEMA_VERSION);
    assert_eq!(report["completeness"], "incomplete");
    assert_eq!(report["requested_scope"]["diagnostics"], "limited");
    assert_eq!(report["target"]["environment"], "staging");
    assert_eq!(report["target"]["tenant_supplied"], true);
    assert_eq!(report["counts"]["pass"], 1);
    assert_eq!(report["counts"]["withheld"], 4);
    assert_eq!(report["counts"]["not_exposed"], 9);
    assert_eq!(
        report["missing_evidence"].as_array().expect("list").len(),
        4
    );
    let version = outcome.finding("process.version");
    assert_eq!(version["status"], "pass");
    assert_eq!(version["evidence"]["server_version"], "0.9.0");
    for id in [
        "process.health",
        "process.ready",
        "ingest.otlp_routes_mounted",
        "query.surface_availability",
    ] {
        let finding = outcome.finding(id);
        assert_eq!(finding["status"], "withheld", "{id}");
        assert_eq!(finding["severity"], "required", "{id}");
    }
    assert_eq!(
        outcome.finding("process.build_match")["status"],
        "not_exposed"
    );
    assert!(!outcome.stdout.contains("secret-key"));
    assert!(!outcome.stdout.contains("tenant-a"));
    assert!(
        outcome.stderr.contains(
            "warning: completeness unknown (limited diagnostics: 4 required check(s) withheld)"
        ),
        "{}",
        outcome.stderr
    );
    assert_only_info(&server).await;
}

#[tokio::test]
async fn the_text_report_says_it_is_limited() {
    let server = server(info()).await;
    let outcome = doctor(at(&server.uri()), OperatorFormat::Text, Env::default()).await;
    assert_eq!(outcome.exit, Exit::Incomplete, "{}", outcome.stderr);
    assert!(
        outcome.stdout.starts_with(&format!(
            "packdb doctor (limited diagnostics): {} (environment staging)\n",
            server.uri()
        )),
        "{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("withheld     process.health"),
        "{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("pass         process.version"),
        "{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.ends_with(
            "completeness: incomplete; 1 pass, 0 warn, 0 fail, 0 error, 4 withheld, 9 not exposed\n"
        ),
        "{}",
        outcome.stdout
    );
    assert_only_info(&server).await;
}

#[tokio::test]
async fn an_unusable_info_answer_is_missing_evidence() {
    for (response, reason) in [
        (
            ResponseTemplate::new(200).set_body_json(json!({"name": "packdb"})),
            "without the service information shape",
        ),
        (
            info().insert_header("x-packdb-partial-causes", "cold_tier_unavailable"),
            "partial result (cold_tier_unavailable)",
        ),
        (
            ResponseTemplate::new(302).insert_header("location", "https://login.invalid/"),
            "redirect (302)",
        ),
        (
            ResponseTemplate::new(429).insert_header("retry-after", "3600"),
            "asked to wait 3600s",
        ),
    ] {
        let server = server(response).await;
        let outcome = doctor(at(&server.uri()), OperatorFormat::Json, Env::default()).await;
        assert_eq!(outcome.exit, Exit::Incomplete, "{}", outcome.stderr);
        assert_eq!(outcome.finding("process.version")["status"], "error");
        assert!(
            outcome.stderr.contains("note: missing server identity"),
            "{}",
            outcome.stderr
        );
        assert!(
            outcome.stderr.contains(reason),
            "{reason}: {}",
            outcome.stderr
        );
        assert!(
            outcome
                .stderr
                .contains("4 required check(s) withheld, 1 could not be collected"),
            "{}",
            outcome.stderr
        );
        assert_only_info(&server).await;
    }
}

#[tokio::test]
async fn rejected_credentials_exit_4_or_5() {
    for (status, exit, kind) in [
        (401, Exit::Unauthenticated, "unauthenticated"),
        (403, Exit::PermissionDenied, "permission_denied"),
    ] {
        let server = server(ResponseTemplate::new(status)).await;
        let outcome = doctor(at(&server.uri()), OperatorFormat::Json, Env::default()).await;
        assert_eq!(outcome.exit, exit, "{}", outcome.stderr);
        assert_eq!(outcome.envelope()["error"]["kind"], kind);
        assert!(
            outcome.stderr.contains(&format!("({status})")),
            "{}",
            outcome.stderr
        );
    }
}

#[tokio::test]
async fn an_unreachable_or_silent_server_exits_6() {
    let refusing = support::refusing_endpoint();
    let refused = doctor(at(&refusing.endpoint), OperatorFormat::Json, Env::default()).await;
    assert_eq!(refused.exit, Exit::Unreachable, "{}", refused.stderr);
    assert_eq!(refused.envelope()["error"]["kind"], "unreachable");
    assert_eq!(refused.envelope()["transport"]["used"], Value::Null);

    let silent = server(info().set_delay(Duration::from_secs(3))).await;
    let started = std::time::Instant::now();
    let outcome = doctor(
        ConnectionArgs {
            timeout: Some(1),
            ..at(&silent.uri())
        },
        OperatorFormat::Json,
        Env::default(),
    )
    .await;
    assert_eq!(outcome.exit, Exit::Unreachable, "{}", outcome.stderr);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert!(
        outcome.stderr.contains("no answer within 1s"),
        "{}",
        outcome.stderr
    );
}
