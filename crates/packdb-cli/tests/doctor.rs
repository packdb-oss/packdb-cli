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

use packdb_cli::{
    Command, ConnectionArgs, DoctorArgs, Environment, Exit, Io, OperatorFormat, Surface, run,
};
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
    fn report(&self) -> Value {
        let envelope: Value = serde_json::from_str(&self.stdout).expect("one JSON object");
        envelope["data"].clone()
    }

    fn finding(&self, id: &str) -> Value {
        self.report()["findings"]
            .as_array()
            .expect("findings")
            .iter()
            .find(|finding| finding["id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("no finding {id}: {}", self.stdout))
    }
}

async fn doctor(
    endpoint: &str,
    surfaces: Vec<Surface>,
    format: OperatorFormat,
    env: Env,
) -> Outcome {
    let command = Command::Doctor(DoctorArgs {
        connection: ConnectionArgs {
            endpoint: Some(endpoint.into()),
            ..ConnectionArgs::default()
        },
        surfaces,
        format,
    });
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

async fn json_doctor(server: &MockServer) -> Outcome {
    doctor(
        &server.uri(),
        Vec::new(),
        OperatorFormat::Json,
        Env::default(),
    )
    .await
}

fn health(status: &str, code: u16) -> ResponseTemplate {
    ResponseTemplate::new(code).set_body_json(json!({
        "status": status, "timestamp": "2026-10-24T00:00:00Z", "version": "0.9.0",
        "uptime_seconds": 12,
        "checks": [{"name": "memory", "status": "healthy", "message": null},
                   {"name": "storage", "status": status, "message": null}]
    }))
}

fn ready(ready: bool) -> ResponseTemplate {
    ResponseTemplate::new(if ready { 200 } else { 503 }).set_body_json(json!({
        "ready": ready, "timestamp": "2026-10-24T00:00:00Z",
        "checks": [{"name": "query_engine", "ready": true, "message": null},
                   {"name": "recovery.wal", "ready": ready, "message": "replaying"}]
    }))
}

async fn mount(server: &MockServer, route: &str, response: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(response)
        .mount(server)
        .await;
}

/// A mock that answers ahead of the healthy defaults.
async fn mount_first(server: &MockServer, route: &str, response: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(response)
        .with_priority(1)
        .mount(server)
        .await;
}

/// Every route doctor reads, answering as a healthy server does.
async fn healthy() -> MockServer {
    let server = MockServer::start().await;
    mount(
        &server,
        "/info",
        ResponseTemplate::new(200).set_body_json(json!({
            "name": "packdb", "version": "0.9.0", "build_time": "unknown",
            "start_time": "2026-10-24T00:00:00Z", "uptime_seconds": 12,
            "environment": "staging"
        })),
    )
    .await;
    mount(&server, "/health/detailed", health("healthy", 200)).await;
    mount(&server, "/ready", ready(true)).await;
    for otlp in ["/v1/metrics", "/v1/logs", "/v1/traces"] {
        mount(
            &server,
            otlp,
            ResponseTemplate::new(405).insert_header("allow", "POST"),
        )
        .await;
    }
    mount(
        &server,
        "/api/v1/query",
        ResponseTemplate::new(400).set_body_json(
            json!({"status": "error", "errorType": "bad_data", "error": "parse error"}),
        ),
    )
    .await;
    mount(
        &server,
        "/loki/api/v1/labels",
        ResponseTemplate::new(200).set_body_json(json!({"status": "success", "data": ["job"]})),
    )
    .await;
    mount(
        &server,
        "/api/search",
        ResponseTemplate::new(400).set_body_json(json!({"error": "invalid TraceQL"})),
    )
    .await;
    mount(
        &server,
        "/jaeger/api/services",
        ResponseTemplate::new(200).set_body_json(json!({"data": ["api"]})),
    )
    .await;
    mount(
        &server,
        "/api/v1/sql",
        ResponseTemplate::new(200).set_body_json(json!({"schema": [], "rows": [[1]]})),
    )
    .await;
    server
}

#[tokio::test]
async fn a_healthy_server_passes_and_lists_what_it_does_not_expose() {
    let server = healthy().await;
    let env = Env::default()
        .with("PACKDB_API_KEY", "secret-key")
        .with("PACKDB_TENANT", "tenant-a");
    let outcome = doctor(&server.uri(), Vec::new(), OperatorFormat::Json, env).await;
    assert_eq!(outcome.exit, Exit::Success, "{}", outcome.stderr);
    let envelope: Value = serde_json::from_str(&outcome.stdout).expect("JSON");
    assert_eq!(envelope["command"], "doctor");
    assert_eq!(envelope["completeness"], "complete");
    let report = outcome.report();
    assert_eq!(report["schema_version"], packdb_cli::REPORT_SCHEMA_VERSION);
    assert_eq!(report["completeness"], "complete");
    assert_eq!(report["target"]["environment"], "staging");
    assert_eq!(report["target"]["tenant_supplied"], true);
    assert_eq!(report["counts"]["fail"], 0);
    assert_eq!(report["counts"]["error"], 0);
    assert_eq!(report["counts"]["not_exposed"], 9);
    for id in [
        "process.health",
        "process.ready",
        "process.version",
        "ingest.signals_enabled",
        "query.surface_availability",
    ] {
        assert_eq!(outcome.finding(id)["status"], "pass", "{id}");
    }
    assert_eq!(
        outcome.finding("process.build_match")["status"],
        "not_exposed"
    );
    assert_eq!(outcome.finding("durability.ages")["status"], "not_exposed");
    assert_eq!(
        outcome.finding("process.version")["evidence"]["server_version"],
        "0.9.0"
    );
    assert!(!outcome.stdout.contains("secret-key"));
    assert!(!outcome.stdout.contains("tenant-a"));
    assert!(outcome.stderr.is_empty(), "{}", outcome.stderr);
    for request in server.received_requests().await.expect("recorded") {
        assert_eq!(request.method.as_str(), "GET", "{}", request.url);
        assert_eq!(request.headers["x-api-key"], "secret-key");
        assert_eq!(request.headers["x-scope-orgid"], "tenant-a");
    }
}

#[tokio::test]
async fn the_text_report_has_one_line_per_finding_and_the_totals() {
    let server = healthy().await;
    let outcome = doctor(
        &server.uri(),
        Vec::new(),
        OperatorFormat::Text,
        Env::default(),
    )
    .await;
    assert_eq!(outcome.exit, Exit::Success, "{}", outcome.stderr);
    assert!(
        outcome.stdout.starts_with(&format!(
            "packdb doctor: {} (environment staging)\n",
            server.uri()
        )),
        "{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("pass         process.health"),
        "{}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("not exposed  process.build_match"),
        "{}",
        outcome.stdout
    );
    assert!(
        outcome
            .stdout
            .ends_with("completeness: complete; 5 pass, 0 warn, 0 fail, 0 error, 9 not exposed\n"),
        "{}",
        outcome.stdout
    );
}

#[tokio::test]
async fn degraded_warns_while_unhealthy_and_not_ready_fail() {
    let server = healthy().await;
    mount_first(&server, "/health/detailed", health("degraded", 200)).await;
    let degraded = json_doctor(&server).await;
    assert_eq!(degraded.exit, Exit::Success, "{}", degraded.stderr);
    assert_eq!(degraded.finding("process.health")["status"], "warn");

    let server = healthy().await;
    mount_first(&server, "/health/detailed", health("unhealthy", 503)).await;
    let unhealthy = json_doctor(&server).await;
    assert_eq!(unhealthy.exit, Exit::Rejected, "{}", unhealthy.stderr);
    let finding = unhealthy.finding("process.health");
    assert_eq!(finding["status"], "fail");
    assert_eq!(
        finding["evidence"]["checks_not_healthy"],
        json!(["storage=unhealthy"])
    );

    let server = healthy().await;
    mount_first(&server, "/ready", ready(false)).await;
    let not_ready = json_doctor(&server).await;
    assert_eq!(not_ready.exit, Exit::Rejected, "{}", not_ready.stderr);
    let finding = not_ready.finding("process.ready");
    assert_eq!(finding["status"], "fail");
    assert_eq!(
        finding["evidence"]["checks_not_ready"],
        json!(["recovery.wal"])
    );
}

#[tokio::test]
async fn an_unproven_surface_is_missing_evidence_not_a_pass() {
    let server = healthy().await;
    mount_first(
        &server,
        "/jaeger/api/services",
        ResponseTemplate::new(404).set_body_string("404 page not found"),
    )
    .await;
    let outcome = json_doctor(&server).await;
    assert_eq!(outcome.exit, Exit::Incomplete, "{}", outcome.stderr);
    let finding = outcome.finding("query.surface_availability");
    assert_eq!(finding["status"], "error");
    assert_eq!(finding["evidence"]["unproven"], json!(["jaeger"]));
    let report = outcome.report();
    assert_eq!(report["completeness"], "incomplete");
    assert_eq!(
        report["missing_evidence"][0]["evidence"],
        "query surface jaeger"
    );
    let envelope: Value = serde_json::from_str(&outcome.stdout).expect("JSON");
    assert_eq!(envelope["completeness"], "unknown");
    assert!(
        outcome
            .stderr
            .contains("note: missing query surface jaeger"),
        "{}",
        outcome.stderr
    );
    assert!(
        outcome.stderr.contains("warning: completeness unknown"),
        "{}",
        outcome.stderr
    );
}

#[tokio::test]
async fn only_the_requested_surfaces_are_probed() {
    let server = healthy().await;
    let outcome = doctor(
        &server.uri(),
        vec![Surface::Promql, Surface::Sql, Surface::Promql],
        OperatorFormat::Json,
        Env::default(),
    )
    .await;
    assert_eq!(outcome.exit, Exit::Success, "{}", outcome.stderr);
    assert_eq!(
        outcome.report()["requested_scope"]["surfaces"],
        json!(["promql", "sql"])
    );
    let paths: Vec<String> = server
        .received_requests()
        .await
        .expect("recorded")
        .iter()
        .map(|request| request.url.path().to_string())
        .collect();
    assert!(
        !paths.iter().any(|path| path.starts_with("/jaeger")),
        "{paths:?}"
    );
    assert!(
        !paths.iter().any(|path| path.starts_with("/loki")),
        "{paths:?}"
    );
}

#[tokio::test]
async fn rejected_credentials_exit_4_or_5() {
    for (status, exit, kind) in [
        (401, Exit::Unauthenticated, "unauthenticated"),
        (403, Exit::PermissionDenied, "permission_denied"),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(status))
            .mount(&server)
            .await;
        let outcome = json_doctor(&server).await;
        assert_eq!(outcome.exit, exit, "{}", outcome.stderr);
        let envelope: Value = serde_json::from_str(&outcome.stdout).expect("JSON");
        assert_eq!(envelope["error"]["kind"], kind);
        assert_eq!(envelope["data"]["completeness"], "incomplete");
        assert!(
            outcome.stderr.contains(&format!("({status})")),
            "{}",
            outcome.stderr
        );
    }
}

#[tokio::test]
async fn an_unreachable_server_exits_6() {
    let refusing = support::refusing_endpoint();
    let outcome = doctor(
        &refusing.endpoint,
        Vec::new(),
        OperatorFormat::Json,
        Env::default(),
    )
    .await;
    assert_eq!(outcome.exit, Exit::Unreachable, "{}", outcome.stderr);
    let envelope: Value = serde_json::from_str(&outcome.stdout).expect("JSON");
    assert_eq!(envelope["error"]["kind"], "unreachable");
    assert_eq!(envelope["transport"]["used"], Value::Null);
}

#[tokio::test]
async fn a_server_that_answers_nothing_is_not_asked_again() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_delay(std::time::Duration::from_secs(3)))
        .mount(&server)
        .await;
    let command = Command::Doctor(DoctorArgs {
        connection: ConnectionArgs {
            endpoint: Some(server.uri()),
            timeout: Some(1),
            ..ConnectionArgs::default()
        },
        surfaces: Vec::new(),
        format: OperatorFormat::Json,
    });
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let started = std::time::Instant::now();
    let exit = run(
        command,
        Io {
            stdout: &mut stdout,
            stderr: &mut stderr,
            env: &Env::default(),
        },
    )
    .await;
    assert_eq!(exit, Exit::Unreachable);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(server.received_requests().await.expect("recorded").len(), 1);
    let stderr = String::from_utf8(stderr).expect("utf-8");
    assert!(stderr.contains("no answer within 1s"), "{stderr}");
    assert!(stderr.contains("was not requested"), "{stderr}");
}

#[tokio::test]
async fn a_throttled_read_is_sent_again_and_a_persistent_throttle_is_missing_evidence() {
    let server = healthy().await;
    Mock::given(method("GET"))
        .and(path("/ready"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    let recovered = json_doctor(&server).await;
    assert_eq!(recovered.exit, Exit::Success, "{}", recovered.stderr);
    assert_eq!(recovered.finding("process.ready")["status"], "pass");

    let server = healthy().await;
    Mock::given(method("GET"))
        .and(path("/ready"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .with_priority(1)
        .expect(3)
        .mount(&server)
        .await;
    let throttled = json_doctor(&server).await;
    assert_eq!(throttled.exit, Exit::Incomplete, "{}", throttled.stderr);
    assert_eq!(throttled.finding("process.ready")["status"], "error");
    assert!(
        throttled
            .stderr
            .contains("throttled (429) on each of 3 attempt(s)"),
        "{}",
        throttled.stderr
    );
}

#[tokio::test]
async fn redirects_partial_answers_and_oversized_bodies_are_not_evidence() {
    let server = healthy().await;
    mount_first(
        &server,
        "/ready",
        ResponseTemplate::new(302).insert_header("location", "https://login.invalid/"),
    )
    .await;
    mount_first(
        &server,
        "/health/detailed",
        health("healthy", 200).insert_header("x-packdb-partial-causes", "cold_tier_unavailable"),
    )
    .await;
    mount_first(
        &server,
        "/loki/api/v1/labels",
        ResponseTemplate::new(200).set_body_string("x".repeat(9 * 1024 * 1024)),
    )
    .await;
    let outcome = json_doctor(&server).await;
    assert_eq!(outcome.exit, Exit::Incomplete, "{}", outcome.stderr);
    for id in [
        "process.ready",
        "process.health",
        "query.surface_availability",
    ] {
        assert_eq!(outcome.finding(id)["status"], "error", "{id}");
    }
    assert!(
        outcome.stderr.contains("redirect (302)"),
        "{}",
        outcome.stderr
    );
    assert!(
        outcome
            .stderr
            .contains("partial result (cold_tier_unavailable)"),
        "{}",
        outcome.stderr
    );
    assert!(
        outcome.stderr.contains("more than 8388608 bytes"),
        "{}",
        outcome.stderr
    );
    let followed = server
        .received_requests()
        .await
        .expect("recorded")
        .iter()
        .any(|request| request.url.host_str() == Some("login.invalid"));
    assert!(!followed);
}

#[tokio::test]
async fn disagreeing_versions_and_unmounted_otlp_are_unknown() {
    let server = healthy().await;
    mount_first(
        &server,
        "/info",
        ResponseTemplate::new(200).set_body_json(json!({"name": "packdb", "version": "0.8.0"})),
    )
    .await;
    mount_first(&server, "/v1/logs", ResponseTemplate::new(404)).await;
    let outcome = json_doctor(&server).await;
    assert_eq!(outcome.exit, Exit::Incomplete, "{}", outcome.stderr);
    assert_eq!(outcome.finding("process.version")["status"], "error");
    let otlp = outcome.finding("ingest.signals_enabled");
    assert_eq!(otlp["status"], "error");
    assert_eq!(otlp["evidence"]["unproven"], json!(["logs"]));
    assert_eq!(otlp["evidence"]["proven"], json!(["metrics", "traces"]));
}

#[tokio::test]
async fn the_tenant_and_credential_reach_every_probe() {
    let server = healthy().await;
    let outcome = doctor(
        &server.uri(),
        vec![Surface::Sql],
        OperatorFormat::Json,
        Env::default()
            .with("PACKDB_TOKEN", "token-1")
            .with("PACKDB_TENANT", "t"),
    )
    .await;
    assert_eq!(outcome.exit, Exit::Success, "{}", outcome.stderr);
    for request in server.received_requests().await.expect("recorded") {
        assert_eq!(request.headers["authorization"], "Bearer token-1");
        assert_eq!(request.headers["x-scope-orgid"], "t");
    }
}
