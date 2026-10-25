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

//! `packdb support-bundle` through [`packdb_cli::run`] against a mock server.

#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

mod support;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use packdb_cli::{
    Command, ConnectionArgs, Environment, Exit, Io, OperatorFormat, SupportBundleArgs, run,
};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const API_KEY: &str = "sk-live-bundle-test-0123456789";

struct Env(HashMap<String, String>);

impl Environment for Env {
    fn var(&self, name: &str) -> Option<String> {
        self.0.get(name).cloned()
    }

    fn read_file(&self, _: &Path, _: u64) -> std::io::Result<Vec<u8>> {
        Err(std::io::Error::from(std::io::ErrorKind::NotFound))
    }
}

fn env(key: &str) -> Env {
    Env(HashMap::from([(
        "PACKDB_API_KEY".to_string(),
        key.to_string(),
    )]))
}

struct Outcome {
    exit: Exit,
    stdout: String,
    stderr: String,
}

impl Outcome {
    fn status(&self, id: &str) -> String {
        let envelope: Value = serde_json::from_str(&self.stdout).expect("one JSON object");
        envelope["data"]["findings"]
            .as_array()
            .expect("findings")
            .iter()
            .find(|finding| finding["id"] == id)
            .and_then(|finding| finding["status"].as_str())
            .unwrap_or_else(|| panic!("no finding {id}: {}", self.stdout))
            .to_string()
    }
}

/// A fresh directory for one test's archive.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("packdb-bundle-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

async fn bundle(endpoint: &str, output: &Path) -> Outcome {
    bundle_with_key(endpoint, output, API_KEY).await
}

async fn bundle_with_key(endpoint: &str, output: &Path, key: &str) -> Outcome {
    let command = Command::SupportBundle(SupportBundleArgs {
        connection: ConnectionArgs {
            endpoint: Some(endpoint.into()),
            ..ConnectionArgs::default()
        },
        output: Some(output.to_path_buf()),
        format: OperatorFormat::Json,
    });
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let exit = run(
        command,
        Io {
            stdout: &mut stdout,
            stderr: &mut stderr,
            env: &env(key),
        },
    )
    .await;
    Outcome {
        exit,
        stdout: String::from_utf8(stdout).expect("utf-8"),
        stderr: String::from_utf8(stderr).expect("utf-8"),
    }
}

fn diagnostics() -> Value {
    json!({
        "schema_version": "packdb.server.diagnostics.v1",
        "health": {"status": "healthy", "timestamp": "t", "version": "0.2.0",
                   "uptime_seconds": 1, "checks": [{"name": "storage", "status": "healthy"}]},
        "readiness": {"ready": true, "timestamp": "t", "checks": [{"name": "storage", "ready": true}]},
        "routes": {"otlp": ["metrics", "logs", "traces"], "query": ["sql"]}
    })
}

async fn server(info: Value, diagnostics: Value) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(info))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/health/diagnostics"))
        .respond_with(ResponseTemplate::new(200).set_body_json(diagnostics))
        .mount(&server)
        .await;
    server
}

fn unpacked(archive: &Path) -> Vec<u8> {
    zstd::decode_all(std::fs::read(archive).expect("archive").as_slice()).expect("zstd")
}

#[tokio::test]
async fn a_bundle_holds_the_server_answers_and_never_the_credential() {
    let server = server(json!({"name": "packdb", "version": "0.2.0"}), diagnostics()).await;
    let dir = scratch("clean");
    let output = dir.join("b.tar.zst");
    let outcome = bundle(&server.uri(), &output).await;
    // Doctor withholds checks, so the bundle it carries is incomplete.
    assert_eq!(outcome.exit, Exit::Incomplete, "{}", outcome.stderr);
    for id in [
        "bundle.doctor_report",
        "bundle.server_info",
        "bundle.server_diagnostics",
        "bundle.client_environment",
        "bundle.archive",
    ] {
        assert_eq!(outcome.status(id), "pass", "{id}");
    }
    let tar = unpacked(&output);
    let text = String::from_utf8_lossy(&tar);
    for name in [
        "manifest.json",
        "checksums.sha256",
        "doctor/report.json",
        "server/info.json",
        "server/diagnostics.json",
        "client/environment.json",
        "packdb.cli.support-bundle.v1",
        "PACKDB_API_KEY",
    ] {
        assert!(text.contains(name), "{name}");
    }
    assert!(
        !text.contains(API_KEY),
        "the credential reached the archive"
    );
    assert!(!outcome.stdout.contains(API_KEY) && !outcome.stderr.contains(API_KEY));
    for request in server.received_requests().await.expect("recorded") {
        assert_eq!(request.method.as_str(), "GET");
    }
    std::fs::remove_dir_all(&dir).expect("clean up");
}

#[tokio::test]
async fn a_server_answer_carrying_a_secret_is_left_out() {
    for (info, diagnostics_body, withheld) in [
        (
            json!({"name": "packdb", "version": "0.2.0", "environment": API_KEY}),
            diagnostics(),
            "bundle.server_info",
        ),
        (
            json!({"name": "packdb", "version": "0.2.0"}),
            {
                let mut body = diagnostics();
                body["health"]["checks"][0]["message"] =
                    json!("-----BEGIN PRIVATE KEY-----\nMIIEvQ");
                body
            },
            "bundle.server_diagnostics",
        ),
    ] {
        let server = server(info, diagnostics_body).await;
        let dir = scratch("secret");
        let output = dir.join("b.tar.zst");
        let outcome = bundle(&server.uri(), &output).await;
        assert_eq!(outcome.exit, Exit::Incomplete, "{}", outcome.stderr);
        assert_eq!(outcome.status(withheld), "withheld", "{withheld}");
        assert_eq!(outcome.status("bundle.archive"), "pass");
        let tar = unpacked(&output);
        let text = String::from_utf8_lossy(&tar);
        assert!(!text.contains(API_KEY), "{withheld}");
        assert!(!text.contains("BEGIN PRIVATE KEY"), "{withheld}");
        assert!(
            outcome.stderr.contains("the secret scan found"),
            "{}",
            outcome.stderr
        );
        std::fs::remove_dir_all(&dir).expect("clean up");
    }
}

#[tokio::test]
async fn an_existing_output_is_refused_before_anything_is_sent() {
    let server = MockServer::start().await;
    let dir = scratch("exists");
    let output = dir.join("b.tar.zst");
    std::fs::write(&output, b"keep me").expect("existing file");
    let outcome = bundle(&server.uri(), &output).await;
    assert_eq!(outcome.exit, Exit::Usage);
    assert_eq!(std::fs::read(&output).expect("read"), b"keep me");
    assert!(
        server
            .received_requests()
            .await
            .expect("recorded")
            .is_empty()
    );
    std::fs::remove_dir_all(&dir).expect("clean up");
}

#[tokio::test]
async fn an_unreachable_server_still_gets_a_bundle_and_exits_6() {
    let refusing = support::refusing_endpoint();
    let dir = scratch("unreachable");
    let output = dir.join("b.tar.zst");
    let outcome = bundle(&refusing.endpoint, &output).await;
    assert_eq!(outcome.exit, Exit::Unreachable, "{}", outcome.stderr);
    assert_eq!(outcome.status("bundle.server_info"), "error");
    assert_eq!(outcome.status("bundle.doctor_report"), "pass");
    assert!(
        output.exists(),
        "the client side is still bundled for support"
    );
    std::fs::remove_dir_all(&dir).expect("clean up");
}

#[tokio::test]
async fn an_unwritable_destination_exits_8() {
    let server = server(json!({"name": "packdb", "version": "0.2.0"}), diagnostics()).await;
    let dir = scratch("unwritable");
    let output = dir.join("missing-directory").join("b.tar.zst");
    let outcome = bundle(&server.uri(), &output).await;
    assert_eq!(outcome.exit, Exit::OutputFailed, "{}", outcome.stderr);
    assert_eq!(outcome.status("bundle.archive"), "error");
    std::fs::remove_dir_all(&dir).expect("clean up");
}

#[tokio::test]
async fn a_short_or_escaped_credential_never_reaches_the_archive() {
    for key in ["k3y", r#"q"u\o-te"#] {
        let mut diagnostics_body = diagnostics();
        diagnostics_body["health"]["checks"][0]["message"] = json!(format!("echo {key} echo"));
        let server = server(
            json!({"name": "packdb", "version": "0.2.0"}),
            diagnostics_body,
        )
        .await;
        let dir = scratch("escaped");
        let output = dir.join("b.tar.zst");
        let outcome = bundle_with_key(&server.uri(), &output, key).await;
        assert_eq!(
            outcome.status("bundle.server_diagnostics"),
            "withheld",
            "{key}"
        );
        assert_eq!(outcome.status("bundle.archive"), "pass", "{key}");
        let tar = unpacked(&output);
        let escaped = serde_json::to_string(key).expect("json");
        for form in [key, escaped.trim_matches('"')] {
            assert!(
                !tar.windows(form.len())
                    .any(|window| window == form.as_bytes()),
                "{form} reached the archive"
            );
        }
        std::fs::remove_dir_all(&dir).expect("clean up");
    }
}
