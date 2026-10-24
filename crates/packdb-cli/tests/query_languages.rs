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

//! `packdb query promql|logql|traceql` and `packdb trace` through
//! [`packdb_cli::run`] against a mock server.

#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::collections::HashMap;
use std::path::Path;

mod support;

use packdb_cli::{
    Command, ConnectionArgs, Environment, Exit, Io, LogDirection, LogqlArgs, PromqlArgs,
    QueryCommand, QueryFormat, TraceArgs, TraceqlArgs, run,
};
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path, query_param, query_param_is_missing};
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
}

fn connection(endpoint: &str) -> ConnectionArgs {
    ConnectionArgs {
        endpoint: Some(endpoint.into()),
        ..ConnectionArgs::default()
    }
}

async fn execute(command: Command, env: Env) -> Outcome {
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

fn promql(endpoint: &str, format: QueryFormat) -> PromqlArgs {
    PromqlArgs {
        expr: "up".into(),
        connection: connection(endpoint),
        time: None,
        start: None,
        end: None,
        step: None,
        format,
    }
}

fn logql(endpoint: &str, format: QueryFormat) -> LogqlArgs {
    LogqlArgs {
        expr: "{job=\"api\"}".into(),
        connection: connection(endpoint),
        time: None,
        start: None,
        end: None,
        step: None,
        limit: None,
        direction: None,
        format,
    }
}

fn traceql(endpoint: &str, format: QueryFormat) -> TraceqlArgs {
    TraceqlArgs {
        query: "{ .service.name = \"api\" }".into(),
        connection: connection(endpoint),
        start: None,
        end: None,
        limit: None,
        format,
    }
}

fn trace(endpoint: &str, id: &str) -> TraceArgs {
    TraceArgs {
        trace_id: id.into(),
        connection: connection(endpoint),
        start: None,
        end: None,
        format: QueryFormat::Json,
    }
}

fn query(language: QueryCommand) -> Command {
    Command::Query { language }
}

fn ok(body: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(body)
}

fn vector() -> Value {
    json!({"status": "success", "data": {"resultType": "vector", "result": [
        {"metric": {"__name__": "up", "job": "api"}, "value": [1700000000.5, "1"]}
    ]}})
}

/// A server that answers nothing: any request it receives fails the test.
async fn silent() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ok(json!({})))
        .expect(0)
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn promql_sends_credentials_and_tenant_and_reports_unknown_completeness() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/query"))
        .and(query_param("query", "up"))
        .and(query_param_is_missing("time"))
        .and(header("x-api-key", "secret-key"))
        .and(header("x-scope-orgid", "tenant-a"))
        .respond_with(ok(vector()))
        .expect(1)
        .mount(&server)
        .await;
    let env = Env::default()
        .with("PACKDB_API_KEY", "secret-key")
        .with("PACKDB_TENANT", "tenant-a");
    let outcome = execute(
        query(QueryCommand::Promql(promql(
            &server.uri(),
            QueryFormat::Csv,
        ))),
        env,
    )
    .await;
    assert_eq!(outcome.exit, Exit::Incomplete, "{}", outcome.stderr);
    let lines: Vec<&str> = outcome.stdout.lines().collect();
    assert_eq!(lines[0], "labels,timestamp,value", "{}", outcome.stdout);
    assert!(lines[1].contains("job=\"\"api\"\""), "{}", outcome.stdout);
    assert!(lines[1].contains("1700000000.5"), "{}", outcome.stdout);
    assert!(
        outcome.stderr.contains("warning: completeness unknown"),
        "{}",
        outcome.stderr
    );
    assert!(!outcome.stderr.contains("secret-key"), "{}", outcome.stderr);
}

#[tokio::test]
async fn promql_range_sends_the_window_in_seconds_and_the_step() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/query_range"))
        .and(query_param("start", "1700000000"))
        .and(query_param("end", "1700000060.25"))
        .and(query_param("step", "15000ms"))
        .respond_with(ok(
            json!({"status": "success", "data": {"resultType": "matrix", "result": [
                {"metric": {"job": "api"}, "values": [[1700000000, "1"], [1700000015, "2"]]}
            ]}}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    let mut args = promql(&server.uri(), QueryFormat::Table);
    args.start = Some("2023-11-14T22:13:20Z".into());
    args.end = Some("1700000060.25".into());
    args.step = Some("15s".into());
    let outcome = execute(query(QueryCommand::Promql(args)), Env::default()).await;
    assert_eq!(outcome.exit, Exit::Incomplete, "{}", outcome.stderr);
    assert!(outcome.stdout.contains("1700000015"), "{}", outcome.stdout);
}

#[tokio::test]
async fn logql_ranges_are_sent_in_nanoseconds_with_their_options() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/loki/api/v1/query_range"))
        .and(query_param("start", "1699996400000000000"))
        .and(query_param("end", "1700000000000000000"))
        .and(query_param_is_missing("step"))
        .and(query_param("limit", "2"))
        .and(query_param("direction", "forward"))
        .respond_with(ok(
            json!({"status": "success", "data": {"resultType": "streams", "result": [
                {"stream": {"job": "api"}, "values": [["1700000000000000001", "GET /health 200"]]}
            ]}}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    let mut args = logql(&server.uri(), QueryFormat::Csv);
    args.start = Some("1699996400".into());
    args.end = Some("1700000000".into());
    args.limit = Some(2);
    args.direction = Some(LogDirection::Forward);
    let outcome = execute(query(QueryCommand::Logql(args)), Env::default()).await;
    assert_eq!(outcome.exit, Exit::Incomplete, "{}", outcome.stderr);
    let lines: Vec<&str> = outcome.stdout.lines().collect();
    assert_eq!(
        lines[0], "labels,timestamp,line,metadata",
        "{}",
        outcome.stdout
    );
    assert!(lines[1].contains("GET /health 200"), "{}", outcome.stdout);
    assert!(
        lines[1].contains("1700000000000000001"),
        "{}",
        outcome.stdout
    );
}

#[tokio::test]
async fn logql_at_an_instant_uses_the_instant_api() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/loki/api/v1/query"))
        .and(query_param("time", "1700000000000000000"))
        .respond_with(ok(vector()))
        .expect(1)
        .mount(&server)
        .await;
    let mut args = logql(&server.uri(), QueryFormat::Json);
    args.expr = "count_over_time({job=\"api\"}[1m])".into();
    args.time = Some("1700000000".into());
    let outcome = execute(query(QueryCommand::Logql(args)), Env::default()).await;
    assert_eq!(outcome.exit, Exit::Incomplete, "{}", outcome.stderr);
    let envelope = outcome.envelope();
    assert_eq!(envelope["command"], "query logql");
    assert_eq!(envelope["completeness"], "unknown");
    assert_eq!(envelope["data"]["data"]["resultType"], "vector");
}

#[tokio::test]
async fn traceql_partial_causes_reach_the_envelope_and_exit_3() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/search"))
        .and(query_param("start", "1700000000"))
        .and(query_param("end", "1700000001"))
        .and(query_param("limit", "5"))
        .respond_with(
            ok(json!({"traces": [{
                "traceID": "ab12", "rootServiceName": "api", "rootTraceName": "GET /",
                "startTimeUnixNano": "1700000000000000000", "durationMs": 12
            }]}))
            .insert_header("x-packdb-partial-causes", "cold_tier_unavailable"),
        )
        .expect(2)
        .mount(&server)
        .await;
    let mut args = traceql(&server.uri(), QueryFormat::Json);
    args.start = Some("1700000000.5".into());
    args.end = Some("1700000000.5".into());
    args.limit = Some(5);
    let outcome = execute(query(QueryCommand::Traceql(args.clone())), Env::default()).await;
    assert_eq!(outcome.exit, Exit::Incomplete, "{}", outcome.stderr);
    let envelope = outcome.envelope();
    assert_eq!(envelope["completeness"], "partial");
    assert_eq!(envelope["partial_causes"], json!(["cold_tier_unavailable"]));
    assert_eq!(envelope["transport"]["used"], "http");
    assert!(
        outcome.stderr.contains("cold_tier_unavailable"),
        "{}",
        outcome.stderr
    );

    args.format = QueryFormat::Csv;
    let table = execute(query(QueryCommand::Traceql(args)), Env::default()).await;
    assert_eq!(table.exit, Exit::Incomplete, "{}", table.stderr);
    let lines: Vec<&str> = table.stdout.lines().collect();
    assert_eq!(
        lines[0], "traceID,rootServiceName,rootTraceName,startTimeUnixNano,durationMs",
        "{}",
        table.stdout
    );
    assert_eq!(lines[1], "ab12,api,GET /,1700000000000000000,12");
}

#[tokio::test]
async fn a_found_trace_without_a_carrier_is_complete() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/traces/ab12"))
        .and(query_param_is_missing("start"))
        .respond_with(ok(json!({"trace": {"resourceSpans": []}})))
        .expect(1)
        .mount(&server)
        .await;
    let outcome = execute(Command::Trace(trace(&server.uri(), "ab12")), Env::default()).await;
    assert_eq!(outcome.exit, Exit::Success, "{}", outcome.stderr);
    let envelope = outcome.envelope();
    assert_eq!(envelope["command"], "trace");
    assert_eq!(envelope["completeness"], "complete");
    assert_eq!(envelope["data"], json!({"trace": {"resourceSpans": []}}));
    assert!(outcome.stderr.is_empty(), "{}", outcome.stderr);
}

#[tokio::test]
async fn a_trace_window_is_widened_to_whole_seconds() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/traces/ab12"))
        .and(query_param("start", "1700000000"))
        .and(query_param("end", "1700000061"))
        .respond_with(
            ok(json!({"trace": {}}))
                .insert_header("x-packdb-partial-causes", "cold_tier_unavailable"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let mut args = trace(&server.uri(), "ab12");
    args.start = Some("1700000000.9".into());
    args.end = Some("1700000060.1".into());
    let outcome = execute(Command::Trace(args), Env::default()).await;
    assert_eq!(outcome.exit, Exit::Incomplete, "{}", outcome.stderr);
    assert_eq!(outcome.envelope()["completeness"], "partial");
}

#[tokio::test]
async fn a_missing_trace_is_reported_as_not_found() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/traces/ff"))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(json!({"error": "trace not found: ff"})),
        )
        .mount(&server)
        .await;
    let outcome = execute(Command::Trace(trace(&server.uri(), "ff")), Env::default()).await;
    assert_eq!(outcome.exit, Exit::Rejected, "{}", outcome.stderr);
    let envelope = outcome.envelope();
    assert_eq!(envelope["error"]["kind"], "not_found");
    let message = envelope["error"]["message"].as_str().expect("a message");
    assert!(message.contains("trace not found: ff"), "{message}");
    assert!(
        outcome.stderr.starts_with("error: ") && outcome.stderr.contains("trace not found: ff"),
        "{}",
        outcome.stderr
    );
}

#[tokio::test]
async fn a_404_that_is_not_the_servers_trace_miss_is_a_server_failure() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/traces/ab12"))
        .respond_with(ResponseTemplate::new(404).set_body_string("404 page not found"))
        .mount(&server)
        .await;
    let outcome = execute(Command::Trace(trace(&server.uri(), "ab12")), Env::default()).await;
    assert_eq!(outcome.exit, Exit::ServerFailure, "{}", outcome.stderr);
    assert_eq!(outcome.envelope()["error"]["kind"], "server_or_protocol");
}

#[tokio::test]
async fn failures_map_to_the_shared_exit_codes() {
    let cases = [
        (
            ResponseTemplate::new(400).set_body_json(
                json!({"status": "error", "errorType": "bad_data", "error": "parse error"}),
            ),
            Exit::Rejected,
            "query_rejected",
        ),
        (
            ResponseTemplate::new(401).set_body_string("missing credentials"),
            Exit::Unauthenticated,
            "unauthenticated",
        ),
        (
            ResponseTemplate::new(403).set_body_string("not yours"),
            Exit::PermissionDenied,
            "permission_denied",
        ),
        (
            ResponseTemplate::new(503).set_body_json(
                json!({"status": "error", "errorType": "timeout", "error": "deadline"}),
            ),
            Exit::ServerFailure,
            "server_or_protocol",
        ),
    ];
    for (response, exit, kind) in cases {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/query"))
            .respond_with(response)
            .mount(&server)
            .await;
        let outcome = execute(
            query(QueryCommand::Promql(promql(
                &server.uri(),
                QueryFormat::Json,
            ))),
            Env::default(),
        )
        .await;
        assert_eq!(outcome.exit, exit, "{kind}: {}", outcome.stderr);
        let envelope = outcome.envelope();
        assert_eq!(envelope["error"]["kind"], kind);
        assert!(envelope["completeness"].is_null(), "{envelope}");
        assert!(outcome.stderr.starts_with("error: "), "{}", outcome.stderr);
    }
}

#[tokio::test]
async fn an_unreachable_server_exits_6() {
    let refusing = support::refusing_endpoint();
    let outcome = execute(
        query(QueryCommand::Traceql(traceql(
            &refusing.endpoint,
            QueryFormat::Json,
        ))),
        Env::default(),
    )
    .await;
    assert_eq!(outcome.exit, Exit::Unreachable, "{}", outcome.stderr);
    assert_eq!(outcome.envelope()["error"]["kind"], "unreachable");
}

#[tokio::test]
async fn usage_errors_exit_2_before_any_request() {
    let server = silent().await;
    let mut bad_time = promql(&server.uri(), QueryFormat::Json);
    bad_time.end = Some("yesterday".into());
    let mut inverted = logql(&server.uri(), QueryFormat::Json);
    inverted.start = Some("now".into());
    inverted.end = Some("now-1h".into());
    let mut bad_step = promql(&server.uri(), QueryFormat::Json);
    bad_step.step = Some("0s".into());
    let mut tabled_trace = trace(&server.uri(), "ab12");
    tabled_trace.format = QueryFormat::Table;
    let cases = [
        (query(QueryCommand::Promql(bad_time)), "--end"),
        (query(QueryCommand::Logql(inverted)), "--start"),
        (query(QueryCommand::Promql(bad_step)), "--step"),
        (Command::Trace(tabled_trace), "--format json"),
    ];
    for (command, mention) in cases {
        let outcome = execute(command, Env::default()).await;
        assert_eq!(outcome.exit, Exit::Usage, "{}", outcome.stderr);
        assert!(outcome.stdout.is_empty(), "{}", outcome.stdout);
        assert!(outcome.stderr.contains(mention), "{}", outcome.stderr);
    }
}

#[tokio::test]
async fn a_non_hex_trace_id_is_refused_before_any_request() {
    let server = silent().await;
    let outcome = execute(
        Command::Trace(trace(&server.uri(), "../api/search")),
        Env::default(),
    )
    .await;
    assert_ne!(outcome.exit, Exit::Success);
    assert!(outcome.stderr.starts_with("error: "), "{}", outcome.stderr);
}

#[tokio::test]
async fn server_warnings_and_infos_reach_stderr_and_the_envelope() {
    let server = MockServer::start().await;
    let mut body = vector();
    body["warnings"] = json!(["PromQL warning: encountered a mix of histograms"]);
    body["infos"] = json!(["PromQL info: metric might not be a counter"]);
    Mock::given(method("GET"))
        .and(path("/api/v1/query"))
        .respond_with(ok(body))
        .mount(&server)
        .await;
    for format in [QueryFormat::Json, QueryFormat::Table] {
        let outcome = execute(
            query(QueryCommand::Promql(promql(&server.uri(), format))),
            Env::default(),
        )
        .await;
        assert_eq!(outcome.exit, Exit::Incomplete, "{}", outcome.stderr);
        assert!(
            outcome
                .stderr
                .contains("warning: server: PromQL warning: encountered a mix of histograms\n"),
            "{}",
            outcome.stderr
        );
        assert!(
            outcome
                .stderr
                .contains("note: server: PromQL info: metric might not be a counter\n"),
            "{}",
            outcome.stderr
        );
        if format == QueryFormat::Json {
            let envelope = outcome.envelope();
            assert_eq!(
                envelope["server_warnings"],
                json!(["PromQL warning: encountered a mix of histograms"])
            );
            assert_eq!(
                envelope["server_infos"],
                json!(["PromQL info: metric might not be a counter"])
            );
            assert_eq!(envelope["completeness"], "unknown");
        }
    }
}

#[tokio::test]
async fn malformed_answers_exit_7_in_every_format_with_nothing_on_stdout() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/query"))
        .respond_with(
            ok(
                json!({"status": "success", "data": {"resultType": "vector", "result": [
                    {"metric": {"job": "api"}}
                ]}}),
            )
            .insert_header("x-packdb-partial-causes", "cold_tier_unavailable"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/search"))
        .respond_with(ok(json!({"traces": [{"traceID": "ab12"}]})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/traces/ab12"))
        .respond_with(ok(json!({"batches": []})))
        .mount(&server)
        .await;
    let mut commands = Vec::new();
    for format in [QueryFormat::Table, QueryFormat::Csv, QueryFormat::Json] {
        commands.push((
            query(QueryCommand::Promql(promql(&server.uri(), format))),
            format,
        ));
        commands.push((
            query(QueryCommand::Traceql(traceql(&server.uri(), format))),
            format,
        ));
    }
    commands.push((
        Command::Trace(trace(&server.uri(), "ab12")),
        QueryFormat::Json,
    ));
    for (command, format) in commands {
        let outcome = execute(command, Env::default()).await;
        assert_eq!(outcome.exit, Exit::ServerFailure, "{}", outcome.stderr);
        assert!(
            outcome.stderr.contains("error: ") && outcome.stderr.contains("malformed response"),
            "{}",
            outcome.stderr
        );
        if format == QueryFormat::Json {
            let envelope = outcome.envelope();
            assert_eq!(envelope["error"]["kind"], "server_or_protocol");
            assert!(envelope["completeness"].is_null(), "{envelope}");
            assert!(envelope["data"].is_null(), "{envelope}");
            assert_eq!(envelope["server_warnings"], json!([]));
        } else {
            assert!(outcome.stdout.is_empty(), "{}", outcome.stdout);
        }
    }
}
