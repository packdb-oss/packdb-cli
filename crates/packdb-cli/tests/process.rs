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

const PACKDB: &str = env!("CARGO_BIN_EXE_packdb");

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

/// Held while a child is spawned and while a pipe's reading end is closed.
///
/// A spawned child holds a copy of every descriptor this process has open
/// until it execs. A closed-pipe case is only deterministic if no such copy
/// of its reading end survives the close, so no spawn may be in flight then.
/// `spawn` returns after the child has exec'd, so holding this across each
/// spawn and each close is enough.
static SPAWN: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn spawn_lock() -> std::sync::MutexGuard<'static, ()> {
    SPAWN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Spawn `command` with the lock held, then wait for its output without it.
fn output(command: &mut Command) -> Output {
    let child = {
        let _spawning = spawn_lock();
        command.spawn().expect("spawn packdb")
    };
    child.wait_with_output().expect("wait for packdb")
}

async fn packdb(args: &[&str], env: &[(&str, &str)]) -> Output {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
    let env: Vec<(String, String)> = env
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    tokio::task::spawn_blocking(move || {
        let mut command = Command::new(PACKDB);
        for variable in VARIABLES {
            command.env_remove(variable);
        }
        output(
            command
                .args(&args)
                .envs(env)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
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
    let output = packdb(&[], &[]).await;
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
    let output = packdb(&["--help"], &[]).await;
    assert_eq!(output.status.code(), Some(0));
    assert!(text(&output.stdout).contains("query"));
}

#[tokio::test(flavor = "multi_thread")]
async fn version_prints_the_package_version_and_exits_0() {
    let output = packdb(&["--version"], &[]).await;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        text(&output.stdout),
        format!("packdb {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(output.stderr.is_empty(), "{}", text(&output.stderr));
}

#[tokio::test(flavor = "multi_thread")]
async fn every_command_has_help_that_exits_0() {
    for command in [
        &["query", "sql"][..],
        &["query", "promql"],
        &["query", "logql"],
        &["query", "traceql"],
        &["trace"],
        &["doctor"],
        &["smoke-test"],
        &["support-bundle"],
        &["usage-report"],
    ] {
        let mut args = command.to_vec();
        args.push("--help");
        let output = packdb(&args, &[]).await;
        let usage = format!("Usage: packdb {}", command.join(" "));
        assert_eq!(output.status.code(), Some(0), "{command:?}");
        assert!(
            text(&output.stdout).contains(&usage),
            "{command:?}: {}",
            text(&output.stdout)
        );
    }
}

/// Bare `packdb` is a client asking for a command, never a server starting up:
/// it exits promptly with `2`, and leaves nothing behind in its working
/// directory or home.
#[tokio::test(flavor = "multi_thread")]
async fn bare_packdb_exits_2_promptly_and_creates_no_files() {
    let cwd = tempfile::tempdir().expect("working directory");
    let home = tempfile::tempdir().expect("home");
    let (cwd_path, home_path) = (cwd.path().to_path_buf(), home.path().to_path_buf());
    let output = tokio::task::spawn_blocking(move || {
        let mut command = Command::new(PACKDB);
        for variable in VARIABLES {
            command.env_remove(variable);
        }
        let spawning = spawn_lock();
        let mut child = command
            .current_dir(&cwd_path)
            .env("HOME", &home_path)
            .env("XDG_CONFIG_HOME", home_path.join(".config"))
            .env("XDG_DATA_HOME", home_path.join(".local/share"))
            .env("XDG_STATE_HOME", home_path.join(".local/state"))
            .env("XDG_CACHE_HOME", home_path.join(".cache"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn packdb");
        drop(spawning);
        // A process still running after this long is serving, not printing
        // help. Stop it so the failure is reported rather than hanging the run.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while child.try_wait().expect("poll packdb").is_none() {
            if std::time::Instant::now() >= deadline {
                child.kill().expect("stop packdb");
                panic!("bare packdb was still running after 30 s");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        child.wait_with_output().expect("collect packdb's output")
    })
    .await
    .expect("join");
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty(), "{}", text(&output.stdout));
    assert!(
        text(&output.stderr).contains("Usage: packdb"),
        "{}",
        text(&output.stderr)
    );
    for dir in [cwd.path(), home.path()] {
        let entries: Vec<_> = std::fs::read_dir(dir)
            .expect("list directory")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert!(entries.is_empty(), "{}: {entries:?}", dir.display());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_secret_cannot_be_passed_as_an_argument_value() {
    let output = packdb(&["query", "sql", "SELECT 1", "--api-key", "secret"], &[]).await;
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
    let output = packdb(
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
        let output = packdb(
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
    let output = packdb(
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
    let output = packdb(
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
    let output = packdb(
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

/// Which of `packdb`'s streams a case makes unwritable.
#[derive(Debug, Clone, Copy)]
enum Closed {
    Stdout,
    Stderr,
}

/// Run `packdb` with one stream a pipe whose reading end is already closed,
/// so every write to it fails with a broken pipe. (A read-only file would not
/// do: the standard library treats `EBADF` on stdout as a closed stdout and
/// discards the write.) The other stream is captured.
async fn with_closed(args: &[&str], closed: Closed) -> Output {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
    tokio::task::spawn_blocking(move || {
        let writer = {
            let _closing = spawn_lock();
            let (reader, writer) = std::io::pipe().expect("pipe");
            drop(reader);
            writer
        };
        let mut command = Command::new(PACKDB);
        for variable in VARIABLES {
            command.env_remove(variable);
        }
        command.args(&args).stdin(Stdio::null());
        match closed {
            Closed::Stdout => command.stdout(writer).stderr(Stdio::piped()),
            Closed::Stderr => command.stdout(Stdio::piped()).stderr(writer),
        };
        output(&mut command)
    })
    .await
    .expect("join")
}

/// What clap renders -- help, the version, a usage error, bare `packdb`'s
/// help -- is output like any other: when its stream cannot be written,
/// `packdb` exits 8 instead of 0 or 2.
#[tokio::test(flavor = "multi_thread")]
async fn help_version_and_usage_errors_on_a_closed_stream_exit_8() {
    for (args, closed) in [
        (&["--help"][..], Closed::Stdout),
        (&["query", "sql", "--help"], Closed::Stdout),
        (&["--version"], Closed::Stdout),
        (&["query", "sql"], Closed::Stderr),
        (&["no-such-command"], Closed::Stderr),
        (&[], Closed::Stderr),
    ] {
        let output = with_closed(args, closed).await;
        assert_eq!(
            output.status.code(),
            Some(8),
            "{args:?} with {closed:?} unwritable"
        );
        if let Closed::Stdout = closed {
            assert!(
                text(&output.stderr).contains("cannot write to stdout"),
                "{args:?}: {}",
                text(&output.stderr)
            );
        }
    }
}

/// A reader that goes away before the result is written: the write fails with
/// a broken pipe, and the command reports it as exit 8 instead of succeeding.
///
/// The server holds its reply until the test has closed packdb's stdout,
/// so the pipe is closed before packdb can write, whatever the scheduling.
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
        let mut command = Command::new(PACKDB);
        for variable in VARIABLES {
            command.env_remove(variable);
        }
        let spawning = spawn_lock();
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
            .expect("spawn packdb");
        // Still holding the lock: no other spawn can copy the reading end.
        drop(child.stdout.take());
        drop(spawning);
        closed.send(()).expect("release the reply");
        child.wait().expect("wait for packdb")
    })
    .await
    .expect("join");
    server.join().expect("server thread");
    assert_eq!(status.code(), Some(8));
}

#[tokio::test(flavor = "multi_thread")]
async fn help_lists_every_query_language_and_trace() {
    let top = packdb(&["--help"], &[]).await;
    assert!(text(&top.stdout).contains("trace"), "{}", text(&top.stdout));
    let query = packdb(&["query", "--help"], &[]).await;
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
    let output = packdb(
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
    let found = packdb(&["trace", "ab12"], &[("PACKDB_ENDPOINT", &server.uri())]).await;
    assert_eq!(found.status.code(), Some(0), "{}", text(&found.stderr));
    let envelope: Value = serde_json::from_slice(&found.stdout).expect("one JSON object");
    assert_eq!(envelope["completeness"], "complete");
    assert!(found.stderr.is_empty(), "{}", text(&found.stderr));

    let missing = packdb(&["trace", "ff"], &[("PACKDB_ENDPOINT", &server.uri())]).await;
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
        let output = packdb(args, &[("PACKDB_ENDPOINT", &refusing.endpoint)]).await;
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn doctor_is_listed_refuses_csv_and_exits_6_when_unreachable() {
    let help = packdb(&["--help"], &[]).await;
    assert!(
        text(&help.stdout).contains("doctor"),
        "{}",
        text(&help.stdout)
    );
    let csv = packdb(&["doctor", "--format", "csv"], &[]).await;
    assert_eq!(csv.status.code(), Some(2));
    assert!(csv.stdout.is_empty());
    let refusing = refusing_endpoint();
    let output = packdb(&["doctor"], &[("PACKDB_ENDPOINT", &refusing.endpoint)]).await;
    assert_eq!(output.status.code(), Some(6), "{}", text(&output.stderr));
    assert!(
        text(&output.stdout).starts_with("packdb doctor (limited diagnostics): "),
        "{}",
        text(&output.stdout)
    );
    assert!(
        text(&output.stderr).contains("error: /info: cannot connect"),
        "{}",
        text(&output.stderr)
    );
}

#[tokio::test]
async fn usage_report_is_listed_refuses_csv_and_exits_6_when_unreachable() {
    let help = packdb(&["--help"], &[]).await;
    assert!(
        text(&help.stdout).contains("usage-report"),
        "{}",
        text(&help.stdout)
    );
    let csv = packdb(&["usage-report", "--format", "csv"], &[]).await;
    assert_eq!(csv.status.code(), Some(2));
    assert!(csv.stdout.is_empty());
    let refusing = refusing_endpoint();
    let output = packdb(
        &["usage-report"],
        &[("PACKDB_ENDPOINT", &refusing.endpoint)],
    )
    .await;
    assert_eq!(output.status.code(), Some(6), "{}", text(&output.stderr));
    assert!(
        text(&output.stdout).starts_with("packdb usage-report: "),
        "{}",
        text(&output.stdout)
    );
    assert!(
        text(&output.stderr).contains("error: /health/usage: cannot connect"),
        "{}",
        text(&output.stderr)
    );
}
