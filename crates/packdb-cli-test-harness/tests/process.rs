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

use std::process::{Command, Output};

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
    let closed = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let endpoint = format!("http://{}", closed.local_addr().expect("addr"));
    drop(closed);
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
