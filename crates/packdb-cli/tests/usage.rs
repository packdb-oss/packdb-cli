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

//! `packdb usage-report` through [`packdb_cli::run`] against a mock server.

#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

mod support;

use std::collections::HashMap;
use std::path::Path;

use packdb_cli::{
    Command, ConnectionArgs, Environment, Exit, Io, OperatorFormat, UsageReportArgs, run,
};
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Default)]
struct Env(HashMap<String, String>);

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

async fn usage_report(endpoint: &str, format: OperatorFormat, env: Env) -> Outcome {
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let exit = run(
        Command::UsageReport(UsageReportArgs {
            connection: ConnectionArgs {
                endpoint: Some(endpoint.into()),
                ..ConnectionArgs::default()
            },
            format,
        }),
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

fn usage() -> Value {
    json!({
        "schema_version": "packdb.server.usage.v1",
        "observed_at": "2026-10-26T00:00:00Z",
        "memory": {"scope": "process", "period": "current", "value": {
            "resident_bytes": 4096, "budget_bytes": 65536, "hot_resident_bytes": 4096,
            "warm_resident_bytes": 0, "cold_resident_bytes": 0, "external_bytes": 512}},
        "ingest": {"scope": "process", "period": "since_process_start",
                   "value": {"accepted_points": 1200}},
        "storage": {"scope": "tenant", "period": "current", "tenant_id": 0, "value": {
            "tiers": {"hot": {"logical_bytes": 900, "stored_bytes": 900},
                      "warm": {"logical_bytes": 2000, "stored_bytes": 700}},
            "hot_metric_names": 3, "hot_metric_rows": 40}},
    })
}

async fn server(status: u16, body: Value) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/health/usage"))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn every_measurement_keeps_its_scope_and_period_and_the_report_is_complete() {
    let server = server(200, usage()).await;
    let outcome = usage_report(&server.uri(), OperatorFormat::Json, Env::default()).await;
    assert_eq!(outcome.exit, Exit::Success, "{}", outcome.stderr);
    let envelope = outcome.envelope();
    assert_eq!(envelope["command"], "usage-report");
    assert_eq!(envelope["completeness"], "complete");
    assert_eq!(envelope["data"]["report_kind"], "usage-report");
    for (id, scope, period) in [
        ("usage.memory", "process", "current"),
        ("usage.ingest", "process", "since_process_start"),
        ("usage.storage", "tenant", "current"),
    ] {
        let finding = outcome.finding(id);
        assert_eq!(finding["status"], "pass", "{id}");
        assert_eq!(finding["evidence"]["scope"], scope, "{id}");
        assert_eq!(finding["evidence"]["period"], period, "{id}");
    }
    assert_eq!(
        outcome.finding("usage.ingest")["evidence"]["value"]["accepted_points"],
        1200
    );
    assert_eq!(outcome.finding("usage.storage")["evidence"]["tenant_id"], 0);
    for id in [
        "usage.ingest_by_signal",
        "usage.ingest_by_tenant",
        "usage.windowed_totals",
        "usage.queries",
        "usage.limits",
    ] {
        assert_eq!(outcome.finding(id)["status"], "not_exposed", "{id}");
    }
    let requests = server.received_requests().await.expect("recorded");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method.as_str(), "GET");
}

#[tokio::test]
async fn an_unavailable_value_is_missing_evidence_never_zero() {
    let mut body = usage();
    body["storage"]["value"] = Value::Null;
    body["storage"]["unavailable_reason"] = json!("the tenant has no store yet");
    let server = server(200, body).await;
    let outcome = usage_report(&server.uri(), OperatorFormat::Json, Env::default()).await;
    assert_eq!(outcome.exit, Exit::Incomplete, "{}", outcome.stderr);
    let storage = outcome.finding("usage.storage");
    assert_eq!(storage["status"], "error");
    assert!(storage["evidence"].get("value").is_none());
    let envelope = outcome.envelope();
    assert_eq!(envelope["completeness"], "unknown");
    assert!(
        envelope["data"]["missing_evidence"][0]["reason"]
            .as_str()
            .expect("reason")
            .contains("the tenant has no store yet")
    );
    assert!(
        outcome
            .stderr
            .contains("note: missing tenant storage: the server reports it unavailable"),
        "{}",
        outcome.stderr
    );
    assert_eq!(outcome.finding("usage.memory")["status"], "pass");
}

#[tokio::test]
async fn a_server_without_the_usage_route_withholds_every_measurement() {
    let server = server(404, json!({"error": "not found"})).await;
    let outcome = usage_report(&server.uri(), OperatorFormat::Json, Env::default()).await;
    assert_eq!(outcome.exit, Exit::Incomplete, "{}", outcome.stderr);
    for id in ["usage.memory", "usage.ingest", "usage.storage"] {
        assert_eq!(outcome.finding(id)["status"], "withheld", "{id}");
    }
    assert_eq!(outcome.finding("usage.queries")["status"], "not_exposed");
}

#[tokio::test]
async fn a_malformed_answer_is_uncollected_evidence() {
    let mut body = usage();
    body["ingest"]["period"] = json!("current");
    let server = server(200, body).await;
    let outcome = usage_report(&server.uri(), OperatorFormat::Json, Env::default()).await;
    assert_eq!(outcome.exit, Exit::Incomplete, "{}", outcome.stderr);
    for id in ["usage.memory", "usage.ingest", "usage.storage"] {
        assert_eq!(outcome.finding(id)["status"], "error", "{id}");
    }
    assert!(
        outcome.stderr.contains("ingest scope or period"),
        "{}",
        outcome.stderr
    );
}

#[tokio::test]
async fn credentials_and_tenant_are_sent_and_never_printed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/health/usage"))
        .and(header("x-api-key", "usage-secret-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(usage()))
        .mount(&server)
        .await;
    let env = Env(HashMap::from([(
        "PACKDB_API_KEY".to_string(),
        "usage-secret-key".to_string(),
    )]));
    let outcome = usage_report(&server.uri(), OperatorFormat::Text, env).await;
    assert_eq!(outcome.exit, Exit::Success, "{}", outcome.stderr);
    assert!(
        outcome.stdout.contains("1200 points accepted"),
        "{}",
        outcome.stdout
    );
    assert!(
        outcome
            .stdout
            .contains("tenant 0: 1600 bytes stored across 2 tier(s)"),
        "{}",
        outcome.stdout
    );
    assert!(!outcome.stdout.contains("usage-secret-key"));
    assert!(!outcome.stderr.contains("usage-secret-key"));
}

#[tokio::test]
async fn refused_or_unreachable_servers_keep_their_exit_codes() {
    for (status, exit) in [(401, Exit::Unauthenticated), (403, Exit::PermissionDenied)] {
        let server = server(status, json!({"error": "no"})).await;
        let outcome = usage_report(&server.uri(), OperatorFormat::Json, Env::default()).await;
        assert_eq!(outcome.exit, exit, "{status}: {}", outcome.stderr);
    }
    let refusing = support::refusing_endpoint();
    let outcome = usage_report(&refusing.endpoint, OperatorFormat::Json, Env::default()).await;
    assert_eq!(outcome.exit, Exit::Unreachable, "{}", outcome.stderr);
    assert_eq!(outcome.envelope()["error"]["kind"], "unreachable");
}
