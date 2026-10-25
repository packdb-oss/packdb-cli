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

//! `packdb smoke-test` through [`packdb_cli::run`] against a mock server.

#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

mod support;

use std::collections::HashMap;
use std::path::Path;

use packdb_cli::{
    Command, ConnectionArgs, Environment, Exit, Io, OperatorFormat, SmokeTestArgs, run,
};
use serde_json::{Value, json};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

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
    fn report(&self) -> Value {
        let envelope: Value = serde_json::from_str(&self.stdout).expect("one JSON object");
        envelope["data"].clone()
    }

    fn status(&self, id: &str) -> String {
        self.report()["findings"]
            .as_array()
            .expect("findings")
            .iter()
            .find(|finding| finding["id"] == id)
            .and_then(|finding| finding["status"].as_str())
            .unwrap_or_else(|| panic!("no finding {id}: {}", self.stdout))
            .to_string()
    }
}

async fn smoke(endpoint: &str, allow_write: bool) -> Outcome {
    let command = Command::SmokeTest(SmokeTestArgs {
        connection: ConnectionArgs {
            endpoint: Some(endpoint.into()),
            ..ConnectionArgs::default()
        },
        allow_write,
        visibility_timeout: 2,
        format: OperatorFormat::Json,
    });
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
    Outcome {
        exit,
        stdout: String::from_utf8(stdout).expect("utf-8"),
        stderr: String::from_utf8(stderr).expect("utf-8"),
    }
}

/// The run marker and trace ID of the fixture a request carried.
fn identifiers(request: &Request) -> (String, String) {
    let body: Value = serde_json::from_slice(&request.body).expect("fixture JSON");
    let run = body["resourceSpans"][0]["resource"]["attributes"]
        .as_array()
        .and_then(|attributes| {
            attributes
                .iter()
                .find(|attribute| attribute["key"] == "packdb.smoke.run_id")
        })
        .and_then(|attribute| attribute["value"]["stringValue"].as_str())
        .expect("run marker")
        .to_string();
    let trace = body["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["traceId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    (run, trace)
}

/// A server that accepts every write and answers every read with this run's
/// data, read from the traces write.
async fn faithful() -> MockServer {
    let server = MockServer::start().await;
    for otlp in ["/v1/metrics", "/v1/logs", "/v1/traces"] {
        Mock::given(method("POST"))
            .and(path(otlp))
            .respond_with(
                ResponseTemplate::new(200).insert_header("content-type", "application/x-protobuf"),
            )
            .mount(&server)
            .await;
    }
    let answers = || {
        move |request: &Request| -> ResponseTemplate {
            let query: HashMap<String, String> = request.url.query_pairs().into_owned().collect();
            let route = request.url.path();
            let body = match route {
                "/api/v1/query" => {
                    let expr = &query["query"];
                    let value = if expr.contains("requests_total") {
                        "7"
                    } else if expr.contains("queue_depth") {
                        "3"
                    } else if expr.contains("batch_size") {
                        "128"
                    } else {
                        "4"
                    };
                    json!({"status": "success", "data": {"resultType": "vector",
                        "result": [{"metric": {}, "value": [1, value]}]}})
                }
                "/loki/api/v1/query_range" => json!({"status": "success", "data": {
                    "resultType": "streams",
                    "result": [{"stream": {}, "values": [["1", "a"], ["2", "b"]]}]}}),
                _ => json!({}),
            };
            ResponseTemplate::new(200).set_body_json(body)
        }
    };
    Mock::given(method("GET"))
        .and(path_regex("^/(api/v1/query|loki/api/v1/query_range)$"))
        .respond_with(answers())
        .mount(&server)
        .await;
    server
}

/// Mount the trace reads for the run the server received.
async fn trace_reads(server: &MockServer) {
    let requests = server.received_requests().await.expect("recorded");
    let traces = requests
        .iter()
        .find(|request| request.url.path() == "/v1/traces")
        .expect("traces write");
    let (_, trace) = identifiers(traces);
    let spans = json!([{}, {}, {}]);
    Mock::given(method("GET"))
        .and(path("/api/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"traces": [
            {"traceID": trace, "rootServiceName": "a", "rootTraceName": "b",
             "startTimeUnixNano": "1", "durationMs": 1}], "metrics": {}})))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/v2/traces/{trace}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"trace": {"batches": [{"scopeSpans": [{"spans": spans}]}]}, "metrics": {}}),
        ))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/jaeger/api/traces/{trace}")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"data": [{"traceID": trace, "spans": spans}]})),
        )
        .mount(server)
        .await;
}

#[tokio::test]
async fn nothing_is_sent_without_allow_write() {
    let server = MockServer::start().await;
    let outcome = smoke(&server.uri(), false).await;
    assert_eq!(outcome.exit, Exit::Usage);
    assert!(outcome.stdout.is_empty());
    assert!(
        outcome.stderr.contains("--allow-write"),
        "{}",
        outcome.stderr
    );
    assert!(
        server
            .received_requests()
            .await
            .expect("recorded")
            .is_empty()
    );
}

#[tokio::test]
async fn every_write_and_read_carries_this_runs_marker() {
    let server = faithful().await;
    // The trace reads answer only for the run the server received, so mount
    // them once the first writes have arrived: a mock that answered any
    // trace ID would prove nothing about run scoping.
    let outcome = {
        let uri = server.uri();
        let run = smoke(&uri, true);
        let mount = async {
            for _ in 0..50 {
                let seen = server.received_requests().await.expect("recorded");
                if seen
                    .iter()
                    .any(|request| request.url.path() == "/v1/traces")
                {
                    trace_reads(&server).await;
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        };
        let (outcome, ()) = tokio::join!(run, mount);
        outcome
    };
    let requests = server.received_requests().await.expect("recorded");
    let writes: Vec<&Request> = requests
        .iter()
        .filter(|request| request.url.path().starts_with("/v1/"))
        .collect();
    assert_eq!(writes.len(), 3, "each signal is written once");
    let (run, trace) = identifiers(
        writes
            .iter()
            .find(|r| r.url.path() == "/v1/traces")
            .expect("traces"),
    );
    for write in &writes {
        assert_eq!(write.headers["content-type"], "application/json");
        assert!(
            String::from_utf8_lossy(&write.body).contains(&run),
            "{}",
            write.url
        );
    }
    let reads: Vec<&Request> = requests
        .iter()
        .filter(|request| !request.url.path().starts_with("/v1/"))
        .collect();
    assert!(reads.iter().any(|read| read.url.path() == "/api/v1/sql"));
    for read in reads {
        let text = format!("{} {}", read.url, String::from_utf8_lossy(&read.body));
        assert!(
            text.contains(&run) || text.contains(&trace),
            "a read not scoped to this run: {text}"
        );
    }
    let report = outcome.report();
    assert_eq!(report["requested_scope"]["run_id"], run.as_str());
    for id in [
        "write.metrics",
        "write.logs",
        "write.traces",
        "read.promql.counter",
        "read.logql.records",
        "read.traceql.search",
        "read.trace.spans",
        "read.jaeger.spans",
    ] {
        assert_eq!(outcome.status(id), "pass", "{id}: {}", outcome.stderr);
    }
}

#[tokio::test]
async fn a_rejection_fails_the_write_and_skips_its_reads() {
    let server = MockServer::start().await;
    // partial_success { rejected: 2, error_message: "x" }
    let rejected = vec![0x0a, 0x05, 0x08, 0x02, 0x12, 0x01, b'x'];
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/x-protobuf")
                .set_body_bytes(rejected),
        )
        .mount(&server)
        .await;
    let outcome = smoke(&server.uri(), true).await;
    assert_eq!(outcome.exit, Exit::Rejected, "{}", outcome.stderr);
    for id in ["write.metrics", "write.logs", "write.traces"] {
        assert_eq!(outcome.status(id), "fail", "{id}");
    }
    assert_eq!(outcome.status("read.promql.counter"), "error");
    assert!(
        server
            .received_requests()
            .await
            .expect("recorded")
            .iter()
            .all(|request| request.method.as_str() == "POST"),
        "nothing is read for a signal whose write was not accepted"
    );
}

#[tokio::test]
async fn a_malformed_acknowledgement_is_never_zero_rejected() {
    for (content_type, body) in [
        ("application/x-protobuf", vec![0xff, 0xff, 0xff]),
        ("application/x-protobuf", vec![0x10, 0x01]),
        ("application/json", b"{\"partialSuccess\":{}}".to_vec()),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", content_type)
                    .set_body_bytes(body.clone()),
            )
            .mount(&server)
            .await;
        let outcome = smoke(&server.uri(), true).await;
        assert_eq!(
            outcome.exit,
            Exit::Incomplete,
            "{content_type} {body:?}: {}",
            outcome.stderr
        );
        assert_eq!(outcome.status("write.metrics"), "error");
        assert!(
            outcome.stderr.contains("acknowledgement"),
            "{}",
            outcome.stderr
        );
    }
}

#[tokio::test]
async fn a_throttled_write_is_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/metrics"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let outcome = smoke(&server.uri(), true).await;
    assert_eq!(outcome.status("write.metrics"), "error");
    assert!(
        outcome.stderr.contains("a write is not retried"),
        "{}",
        outcome.stderr
    );
}

#[tokio::test]
async fn a_refused_credential_stops_writing() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;
    let outcome = smoke(&server.uri(), true).await;
    assert_eq!(outcome.exit, Exit::Unauthenticated, "{}", outcome.stderr);
    let envelope: Value = serde_json::from_str(&outcome.stdout).expect("JSON");
    assert_eq!(envelope["error"]["kind"], "unauthenticated");
    assert_eq!(outcome.status("write.logs"), "error");
}

#[tokio::test]
async fn an_unreachable_server_exits_6() {
    let refusing = support::refusing_endpoint();
    let outcome = smoke(&refusing.endpoint, true).await;
    assert_eq!(outcome.exit, Exit::Unreachable, "{}", outcome.stderr);
    let envelope: Value = serde_json::from_str(&outcome.stdout).expect("JSON");
    assert_eq!(envelope["transport"]["used"], Value::Null);
}

#[tokio::test]
async fn data_that_never_appears_fails_at_the_visibility_timeout() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/query"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"status": "success", "data": {"resultType": "vector", "result": []}}),
        ))
        .mount(&server)
        .await;
    let started = std::time::Instant::now();
    let outcome = smoke(&server.uri(), true).await;
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    assert_eq!(outcome.exit, Exit::Rejected, "{}", outcome.stderr);
    assert_eq!(outcome.status("read.promql.counter"), "fail");
    assert_eq!(outcome.status("write.metrics"), "pass");
}
