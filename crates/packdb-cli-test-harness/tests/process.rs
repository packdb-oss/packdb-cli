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

//! The command contract as a process sees it: stdout, stderr and exit codes.

#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

use std::io::ErrorKind;
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Output, Stdio};

use serde_json::{Value, json};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const HARNESS: &str = env!("CARGO_BIN_EXE_packdb-cli-test-harness");

/// Every variable the commands read, removed so the runner's own environment
/// cannot leak into a case.
const VARIABLES: [&str; 11] = [
    "PACKDB_ENDPOINT",
    "PACKDB_FLIGHT_ENDPOINT",
    "PACKDB_API_KEY",
    "PACKDB_API_KEY_FILE",
    "PACKDB_TOKEN",
    "PACKDB_TOKEN_FILE",
    "PACKDB_BASIC_AUTH",
    "PACKDB_BASIC_AUTH_FILE",
    "PACKDB_TENANT",
    "PACKDB_TIMEOUT",
    "PACKDB_URL",
];

async fn harness(args: &[&str], env: &[(&str, &str)]) -> Output {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
    let env: Vec<(String, String)> = env
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    tokio::task::spawn_blocking(move || {
        let mut command = Command::new(HARNESS);
        for variable in VARIABLES {
            command.env_remove(variable);
        }
        command
            .args(&args)
            .envs(env)
            .output()
            .expect("run the harness")
    })
    .await
    .expect("join")
}

/// An endpoint that refuses connections. Its port is the local end of an open
/// loopback connection: nothing listens on it, and while this is held the
/// system does not assign the port to another socket bound to port 0, as every
/// test server here is. The refusal is checked here, not assumed.
struct Refusing {
    endpoint: String,
    _held: (TcpListener, TcpStream, TcpStream),
}

fn refusing_endpoint() -> Refusing {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let client =
        TcpStream::connect(listener.local_addr().expect("listener addr")).expect("connect");
    let (server, _) = listener.accept().expect("accept");
    let addr = client.local_addr().expect("local addr");
    let refused = TcpStream::connect(addr).expect_err("nothing listens");
    assert_eq!(refused.kind(), ErrorKind::ConnectionRefused, "{refused}");
    Refusing {
        endpoint: format!("http://{addr}"),
        _held: (listener, client, server),
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("utf-8")
}

async fn sql_server(response: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/sql"))
        .respond_with(response)
        .mount(&server)
        .await;
    server
}

#[tokio::test(flavor = "multi_thread")]
async fn no_subcommand_prints_help_on_stderr_and_exits_2() {
    let output = harness(&[], &[]).await;
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(
        text(&output.stderr).contains("Usage"),
        "{}",
        text(&output.stderr)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn help_exits_0() {
    let output = harness(&["--help"], &[]).await;
    assert_eq!(output.status.code(), Some(0));
    assert!(text(&output.stdout).contains("query"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_secret_cannot_be_passed_as_an_argument_value() {
    let output = harness(&["query", "sql", "SELECT 1", "--api-key", "secret"], &[]).await;
    assert_eq!(output.status.code(), Some(2));
    assert!(!text(&output.stdout).contains("secret"));
}

#[tokio::test(flavor = "multi_thread")]
async fn json_on_stdout_warnings_on_stderr_and_exit_3_for_a_partial_result() {
    let server = sql_server(
        ResponseTemplate::new(200)
            .set_body_json(json!({"rows": [{"count": 7}]}))
            .insert_header("x-packdb-partial-causes", "cold_tier_unavailable"),
    )
    .await;
    let output = harness(
        &[
            "query",
            "sql",
            "SELECT count",
            "--format",
            "json",
            "--transport",
            "http",
        ],
        &[("PACKDB_ENDPOINT", &server.uri())],
    )
    .await;
    assert_eq!(output.status.code(), Some(3));
    let envelope: Value =
        serde_json::from_slice(&output.stdout).expect("stdout is one JSON object");
    assert_eq!(envelope["completeness"], "partial");
    assert_eq!(envelope["data"]["rows"], json!([{"count": 7}]));
    let stderr = text(&output.stderr);
    assert!(stderr.contains("warning: incomplete result"), "{stderr}");
    assert!(!stderr.contains('{'), "no JSON on stderr: {stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn each_failure_has_its_exit_code() {
    let cases = [
        (400, "Query error: bad", 1),
        (401, "who are you", 4),
        (403, "not you", 5),
        (503, "warming up", 7),
    ];
    for (status, body, code) in cases {
        let server = sql_server(ResponseTemplate::new(status).set_body_string(body)).await;
        let output = harness(
            &["query", "sql", "SELECT 1", "--transport", "http"],
            &[("PACKDB_ENDPOINT", &server.uri())],
        )
        .await;
        assert_eq!(output.status.code(), Some(code), "{status}");
        assert!(output.stdout.is_empty(), "{status}");
        assert!(text(&output.stderr).contains(body), "{status}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreachable_server_exits_6() {
    let refusing = refusing_endpoint();
    let endpoint = refusing.endpoint.clone();
    let output = harness(
        &[
            "query",
            "sql",
            "SELECT 1",
            "--endpoint",
            &endpoint,
            "--transport",
            "http",
        ],
        &[],
    )
    .await;
    assert_eq!(output.status.code(), Some(6));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_credential_file_is_read_and_its_secret_is_never_printed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/sql"))
        .and(header("authorization", "Bearer secret-token"))
        .respond_with(ResponseTemplate::new(401).set_body_string("token refused"))
        .expect(1)
        .mount(&server)
        .await;
    let file = std::env::temp_dir().join(format!("packdb-cli-token-{}", std::process::id()));
    std::fs::write(&file, "secret-token\n").expect("write token");
    let output = harness(
        &[
            "query",
            "sql",
            "SELECT 1",
            "--endpoint",
            &server.uri(),
            "--token-file",
            file.to_str().expect("utf-8 path"),
            "--format",
            "json",
        ],
        &[],
    )
    .await;
    std::fs::remove_file(&file).expect("remove token");
    assert_eq!(output.status.code(), Some(4));
    assert!(!text(&output.stdout).contains("secret-token"));
    assert!(!text(&output.stderr).contains("secret-token"));
}

#[tokio::test(flavor = "multi_thread")]
async fn conflicting_credentials_exit_2_without_contacting_the_server() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let output = harness(
        &["query", "sql", "SELECT 1"],
        &[
            ("PACKDB_ENDPOINT", &server.uri()),
            ("PACKDB_API_KEY", "secret-a"),
            ("PACKDB_TOKEN", "secret-b"),
        ],
    )
    .await;
    assert_eq!(output.status.code(), Some(2));
    let stderr = text(&output.stderr);
    assert!(stderr.contains("conflicting credentials"), "{stderr}");
    assert!(!stderr.contains("secret-"), "{stderr}");
}

/// A reader that goes away before the result is written: the write fails with
/// a broken pipe, and the command reports it as exit 8 instead of succeeding.
///
/// The server holds its reply until the test has closed the harness's stdout,
/// so the pipe is closed before the harness can write, whatever the scheduling.
#[tokio::test(flavor = "multi_thread")]
async fn a_closed_stdout_exits_8() {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let endpoint = format!("http://{}", listener.local_addr().expect("addr"));
    let (closed, wait_for_closed) = std::sync::mpsc::channel::<()>();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("accept the query");
        let mut request = [0u8; 4096];
        let _ = socket.read(&mut request).expect("read the query");
        wait_for_closed
            .recv()
            .expect("the test closes stdout first");
        let body = r#"{"rows":[{"count":7}]}"#;
        write!(
            socket,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
        .expect("answer the query");
    });
    let status = tokio::task::spawn_blocking(move || {
        let mut command = Command::new(HARNESS);
        for variable in VARIABLES {
            command.env_remove(variable);
        }
        let mut child = command
            .args([
                "query",
                "sql",
                "SELECT count",
                "--transport",
                "http",
                "--endpoint",
                &endpoint,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the harness");
        drop(child.stdout.take());
        closed.send(()).expect("release the reply");
        child.wait().expect("wait for the harness")
    })
    .await
    .expect("join");
    server.join().expect("server thread");
    assert_eq!(status.code(), Some(8));
}

#[tokio::test(flavor = "multi_thread")]
async fn help_lists_every_query_language_and_trace() {
    let top = harness(&["--help"], &[]).await;
    assert!(text(&top.stdout).contains("trace"), "{}", text(&top.stdout));
    let query = harness(&["query", "--help"], &[]).await;
    assert_eq!(query.status.code(), Some(0));
    let help = text(&query.stdout);
    for language in ["sql", "promql", "logql", "traceql"] {
        assert!(help.contains(language), "{language}: {help}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_promql_answer_without_a_carrier_exits_3_as_unknown() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/query"))
        .and(header("x-scope-orgid", "tenant-a"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"status": "success", "data": {"resultType": "scalar", "result": [1700000000, "1"]}}),
        ))
        .mount(&server)
        .await;
    let output = harness(
        &["query", "promql", "1", "--format", "json"],
        &[
            ("PACKDB_ENDPOINT", &server.uri()),
            ("PACKDB_TENANT", "tenant-a"),
        ],
    )
    .await;
    assert_eq!(output.status.code(), Some(3), "{}", text(&output.stderr));
    let envelope: Value =
        serde_json::from_slice(&output.stdout).expect("stdout is one JSON object");
    assert_eq!(envelope["command"], "query promql");
    assert_eq!(envelope["completeness"], "unknown");
    assert!(
        text(&output.stderr).contains("warning: completeness unknown"),
        "{}",
        text(&output.stderr)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_found_trace_exits_0_and_a_missing_one_exits_1() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/traces/ab12"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"trace": {}})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/traces/ff"))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(json!({"error": "trace not found: ff"})),
        )
        .mount(&server)
        .await;
    let found = harness(&["trace", "ab12"], &[("PACKDB_ENDPOINT", &server.uri())]).await;
    assert_eq!(found.status.code(), Some(0), "{}", text(&found.stderr));
    let envelope: Value = serde_json::from_slice(&found.stdout).expect("one JSON object");
    assert_eq!(envelope["completeness"], "complete");
    assert!(found.stderr.is_empty(), "{}", text(&found.stderr));

    let missing = harness(&["trace", "ff"], &[("PACKDB_ENDPOINT", &server.uri())]).await;
    assert_eq!(missing.status.code(), Some(1), "{}", text(&missing.stderr));
    assert!(
        text(&missing.stderr).contains("trace not found: ff"),
        "{}",
        text(&missing.stderr)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn conflicting_or_incomplete_time_arguments_exit_2() {
    // Usage errors send nothing; a request would be refused and exit 6.
    let refusing = refusing_endpoint();
    for args in [
        &[
            "query", "promql", "up", "--time", "now", "--start", "now-1h",
        ][..],
        &[
            "query",
            "logql",
            "{job=\"a\"}",
            "--time",
            "now",
            "--step",
            "1m",
        ][..],
        &["trace", "ab12", "--start", "now-1h"][..],
        &["query", "traceql", "{}", "--start", "tomorrow"][..],
    ] {
        let output = harness(args, &[("PACKDB_ENDPOINT", &refusing.endpoint)]).await;
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn doctor_is_listed_refuses_csv_and_exits_6_when_unreachable() {
    let help = harness(&["--help"], &[]).await;
    assert!(
        text(&help.stdout).contains("doctor"),
        "{}",
        text(&help.stdout)
    );
    let csv = harness(&["doctor", "--format", "csv"], &[]).await;
    assert_eq!(csv.status.code(), Some(2));
    assert!(csv.stdout.is_empty());
    let refusing = refusing_endpoint();
    let output = harness(&["doctor"], &[("PACKDB_ENDPOINT", &refusing.endpoint)]).await;
    assert_eq!(output.status.code(), Some(6), "{}", text(&output.stderr));
    assert!(
        text(&output.stdout).starts_with("packdb doctor: "),
        "{}",
        text(&output.stdout)
    );
    assert!(
        text(&output.stderr).contains("error: /info: cannot connect"),
        "{}",
        text(&output.stderr)
    );
}
