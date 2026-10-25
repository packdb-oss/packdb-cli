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

//! `packdb support-bundle`: collect diagnostics into one archive for support.
//!
//! The bundle holds what `doctor` reads — `GET /info` and the read-only
//! `GET /health/diagnostics` — with the doctor report and a description of the
//! client, never a credential. Every file is scanned for secrets before it is
//! written; a file that matches is left out and named as withheld, and if the
//! manifest itself matched, no archive is written at all. The archive is a
//! deterministic tar compressed with zstd, checked against its manifest after
//! writing, and never replaces an existing file.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use packdb_client::AuthMethod;
use serde_json::{Value, json};

use crate::Io;
use crate::connection::ConnectionArgs;
use crate::doctor::diagnose;
use crate::environment::Environment;
use crate::exit::Exit;
use crate::output::{
    Envelope, ErrorReport, OperatorFormat, ServerNotices, TransportReport, display_endpoint,
};
use crate::probe::{ProbeError, Prober};
use crate::render::{Rendered, deliver};
use crate::report::{Finding, MissingEvidence, Report, Severity, Status};
use crate::secrets::SecretScanner;

/// The bundle's format, recorded in its manifest.
pub const BUNDLE_FORMAT: &str = "packdb.cli.support-bundle.v1";

/// The most the bundle's files may hold together before compression.
const MAX_STAGED_BYTES: usize = 32 * 1024 * 1024;

/// The variables the commands read; the bundle records which are set, never
/// their values.
const VARIABLES: [&str; 10] = [
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
];

/// Arguments of `packdb support-bundle`.
#[derive(Debug, Clone, clap::Args)]
pub struct SupportBundleArgs {
    /// Where to connect and how to authenticate.
    #[command(flatten)]
    pub connection: ConnectionArgs,

    /// Archive to write; must not exist [default: packdb-support-bundle-YYYYMMDDTHHMMSSZ.tar.zst]
    #[arg(long, value_name = "PATH")]
    pub output: Option<PathBuf>,

    /// Output format of the report.
    #[arg(long, value_enum, default_value_t = OperatorFormat::Text)]
    pub format: OperatorFormat,
}

/// The archive that was written.
#[derive(Debug)]
struct Written {
    path: PathBuf,
    bytes: usize,
    sha256: String,
}

fn sha256(bytes: &[u8]) -> String {
    aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn default_output(now: SystemTime) -> PathBuf {
    let stamp: String = humantime::format_rfc3339_seconds(now)
        .to_string()
        .chars()
        .filter(|c| !matches!(c, '-' | ':'))
        .collect();
    PathBuf::from(format!("packdb-support-bundle-{stamp}.tar.zst"))
}

fn credential_kind(method: &AuthMethod) -> &'static str {
    match method {
        AuthMethod::ApiKey(_) => "api_key",
        AuthMethod::BearerToken(_) => "bearer_token",
        AuthMethod::BasicAuth { .. } => "basic_auth",
        _ => "none",
    }
}

pub(crate) async fn run(args: SupportBundleArgs, io: Io<'_>) -> Exit {
    let Io {
        stdout,
        stderr,
        env,
    } = io;
    let usage =
        |message: &str, stdout: &mut dyn std::io::Write, stderr: &mut dyn std::io::Write| {
            deliver(stdout, stderr, &Rendered::error(message), Exit::Usage)
        };
    let started = SystemTime::now();
    let output = args
        .output
        .clone()
        .unwrap_or_else(|| default_output(started));
    // Checked before anything is sent; the write itself refuses to replace a
    // file that appears in the meantime.
    if output.exists() {
        return usage(
            &format!(
                "{} already exists; a bundle never replaces a file",
                output.display()
            ),
            stdout,
            stderr,
        );
    }
    let connection = match args.connection.resolve(env) {
        Ok(connection) => connection,
        Err(error) => return usage(&error.0, stdout, stderr),
    };
    let scanner = match SecretScanner::new(&connection.auth) {
        Ok(scanner) => scanner,
        Err(error) => return usage(&error, stdout, stderr),
    };
    let client = client_environment(env, &connection);
    let endpoint = connection.endpoint.clone();
    let tenant_supplied = connection.tenant.is_some();
    let prober = match Prober::new(connection) {
        Ok(prober) => prober,
        Err(error) => return usage(&error.0, stdout, stderr),
    };
    let doctor = diagnose(&endpoint, tenant_supplied, &prober).await;

    let mut report = Report::new("support_bundle");
    report.started_at = started;
    report
        .target
        .insert("endpoint", display_endpoint(&endpoint).into());
    report
        .target
        .insert("tenant_supplied", tenant_supplied.into());
    report
        .scope
        .insert("output", output.display().to_string().into());
    let doctor_json = doctor.report.to_json();
    let doctor_body = serde_json::to_vec_pretty(&doctor_json).unwrap_or_default();
    let client_body = serde_json::to_vec_pretty(&client).unwrap_or_default();
    let mut files = Vec::new();
    for (path, body, id, what) in [
        (
            "doctor/report.json",
            Some(doctor_body),
            "bundle.doctor_report",
            "the doctor report",
        ),
        (
            "server/info.json",
            doctor.info_body.clone(),
            "bundle.server_info",
            "the server's /info",
        ),
        (
            "server/diagnostics.json",
            doctor.diagnostics_body.clone(),
            "bundle.server_diagnostics",
            "the server's /health/diagnostics",
        ),
        (
            "client/environment.json",
            Some(client_body),
            "bundle.client_environment",
            "the client environment",
        ),
    ] {
        stage(&mut report, &scanner, &mut files, path, body, id, what);
    }
    if !doctor.report.is_complete() {
        report.missing_evidence.push(MissingEvidence {
            evidence: "a complete doctor report".into(),
            reason: "the doctor report included is incomplete: it withholds or lacks required \
                     checks"
                .into(),
        });
    }
    report.finished_at = SystemTime::now();
    let written = write_bundle(&report, &files, &output, &scanner);
    let archive_failed = written.is_err();
    match &written {
        Ok(written) => report.push(
            Finding::new(
                "bundle.archive",
                Severity::Required,
                Status::Pass,
                format!(
                    "wrote {} ({} bytes), verified against its manifest",
                    written.path.display(),
                    written.bytes
                ),
            )
            .with("path", written.path.display().to_string())
            .with("bytes", written.bytes)
            .with("sha256", written.sha256.clone()),
        ),
        Err(reason) => {
            report.uncollected("bundle.archive", Severity::Required, "the archive", reason)
        }
    }

    let mut exit = doctor
        .failures
        .iter()
        .filter(|failure| !(doctor.answered && matches!(failure, ProbeError::Unreachable(_))))
        .fold(report.exit(), |exit, failure| exit.worst(failure.exit()));
    if archive_failed {
        exit = exit.worst(Exit::OutputFailed);
    }
    let error = doctor
        .failures
        .iter()
        .find(|failure| failure.exit() != Exit::Incomplete && failure.exit() == exit)
        .map(|failure| (probe_error_kind(failure), failure.message().to_string()));
    let out = render(&report, &endpoint, args.format, doctor.answered, error);
    deliver(stdout, stderr, &out, exit)
}

/// Scan one file and add it, or record why it was left out.
fn stage(
    report: &mut Report,
    scanner: &SecretScanner,
    files: &mut Vec<(String, Vec<u8>)>,
    path: &str,
    body: Option<Vec<u8>>,
    id: &'static str,
    what: &str,
) {
    let Some(body) = body else {
        report.uncollected(
            id,
            Severity::Required,
            what,
            "the server gave no usable answer",
        );
        return;
    };
    if let Some(class) = scanner.scan(&body) {
        let reason = format!("withheld: the secret scan found {class}; the file was not written");
        report.push(Finding::new(
            id,
            Severity::Required,
            Status::Withheld,
            format!("{what} was left out: {reason}"),
        ));
        report.missing_evidence.push(MissingEvidence {
            evidence: what.to_string(),
            reason,
        });
        return;
    }
    let staged: usize = files.iter().map(|(_, data)| data.len()).sum();
    if staged + body.len() > MAX_STAGED_BYTES {
        report.uncollected(
            id,
            Severity::Required,
            what,
            &format!("the bundle would exceed {MAX_STAGED_BYTES} bytes"),
        );
        return;
    }
    report.push(
        Finding::new(
            id,
            Severity::Required,
            Status::Pass,
            format!("{what} included as {path}"),
        )
        .with("bytes", body.len()),
    );
    files.push((path.to_string(), body));
}

/// Which variables are set and which credential kind is used — never a value.
fn client_environment(env: &dyn Environment, connection: &crate::connection::Connection) -> Value {
    let set: Vec<&str> = VARIABLES
        .iter()
        .copied()
        .filter(|name| env.var(name).is_some())
        .collect();
    json!({
        "tool": {"name": "packdb-cli", "version": env!("CARGO_PKG_VERSION")},
        "platform": {"os": std::env::consts::OS, "arch": std::env::consts::ARCH},
        "endpoint": display_endpoint(&connection.endpoint),
        "flight_endpoint_set": connection.flight_endpoint.is_some(),
        "tenant_supplied": connection.tenant.is_some(),
        "credential": credential_kind(&connection.auth.method),
        "timeout_secs": connection.timeout.as_secs(),
        "variables_set": set,
    })
}

/// Build, write and verify the archive. It goes to a new temporary file beside
/// `output` (a random name, readable only by its owner), is read back and
/// checked against what was staged, and is then moved to `output` without
/// replacing a file that appeared since. Only that temporary file is ever
/// removed, and it is removed whenever the move does not happen.
fn write_bundle(
    report: &Report,
    files: &[(String, Vec<u8>)],
    output: &Path,
    scanner: &SecretScanner,
) -> Result<Written, String> {
    let listed: Vec<Value> = files
        .iter()
        .map(|(path, data)| json!({"path": path, "bytes": data.len(), "sha256": sha256(data)}))
        .collect();
    let manifest = serde_json::to_vec_pretty(&json!({
        "format": BUNDLE_FORMAT,
        "created_at": humantime::format_rfc3339_millis(report.finished_at).to_string(),
        "tool": {"name": "packdb-cli", "version": env!("CARGO_PKG_VERSION")},
        "files": listed,
        "report": report.to_json(),
    }))
    .map_err(|error| format!("the manifest could not be encoded: {error}"))?;
    let checksums: String = files
        .iter()
        .map(|(path, data)| format!("{}  {path}\n", sha256(data)))
        .collect();
    for (name, bytes) in [
        ("manifest.json", &manifest),
        ("checksums.sha256", &checksums.clone().into_bytes()),
    ] {
        if let Some(class) = scanner.scan(bytes) {
            return Err(format!(
                "no archive was written: the secret scan found {class} in {name}"
            ));
        }
    }
    let mut entries = vec![
        ("manifest.json".to_string(), manifest),
        ("checksums.sha256".to_string(), checksums.into_bytes()),
    ];
    entries.extend(files.iter().cloned());
    let tar = tar_entries(&entries)?;
    let archive = zstd::encode_all(tar.as_slice(), 3)
        .map_err(|error| format!("compression failed: {error}"))?;

    let directory = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temporary = tempfile::Builder::new()
        .prefix(".packdb-support-bundle-")
        .suffix(".partial")
        .tempfile_in(directory)
        .map_err(|error| format!("cannot create a file in {}: {error}", directory.display()))?;
    temporary
        .write_all(&archive)
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|error| format!("cannot write the archive: {error}"))?;
    let read = std::fs::read(temporary.path())
        .map_err(|error| format!("cannot read the archive back: {error}"))?;
    let unpacked = zstd::decode_all(read.as_slice())
        .map_err(|error| format!("the written archive does not decompress: {error}"))?;
    if untar(&unpacked)? != entries {
        return Err("the written archive does not match what was staged".into());
    }
    temporary
        .persist_noclobber(output)
        .map_err(|error| format!("cannot create {}: {}", output.display(), error.error))?;
    Ok(Written {
        path: output.to_path_buf(),
        bytes: archive.len(),
        sha256: sha256(&archive),
    })
}

/// A tar of `entries` in order, with fixed mode, owner and time, so the same
/// contents always produce the same bytes.
fn tar_entries(entries: &[(String, Vec<u8>)]) -> Result<Vec<u8>, String> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, data) in entries {
        let mut header = tar::Header::new_ustar();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_entry_type(tar::EntryType::Regular);
        builder
            .append_data(&mut header, name, data.as_slice())
            .map_err(|error| format!("cannot add {name} to the archive: {error}"))?;
    }
    builder
        .into_inner()
        .map_err(|error| format!("cannot finish the archive: {error}"))
}

/// The entries of a tar, in order.
fn untar(tar: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut archive = tar::Archive::new(tar);
    let mut entries = Vec::new();
    for entry in archive
        .entries()
        .map_err(|error| format!("the archive does not read back: {error}"))?
    {
        let mut entry =
            entry.map_err(|error| format!("the archive does not read back: {error}"))?;
        let name = entry
            .path()
            .map_err(|error| format!("an archive entry has no path: {error}"))?
            .to_string_lossy()
            .into_owned();
        let mut data = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut data)
            .map_err(|error| format!("{name} does not read back: {error}"))?;
        entries.push((name, data));
    }
    Ok(entries)
}

fn probe_error_kind(error: &ProbeError) -> &'static str {
    match error {
        ProbeError::Unauthenticated(_) => "unauthenticated",
        ProbeError::PermissionDenied(_) => "permission_denied",
        ProbeError::Unreachable(_) => "unreachable",
        ProbeError::NoEvidence(_) => "server_or_protocol",
    }
}

fn render(
    report: &Report,
    endpoint: &str,
    format: OperatorFormat,
    answered: bool,
    error: Option<(&'static str, String)>,
) -> Rendered {
    let mut out = Rendered::default();
    let warning = (!report.is_complete()).then(|| {
        format!(
            "completeness unknown ({} piece(s) of required evidence missing or withheld)",
            report.missing_evidence.len().max(1)
        )
    });
    match format {
        OperatorFormat::Json => {
            let envelope = Envelope {
                command: "support-bundle",
                endpoint: endpoint.to_string(),
                transport: TransportReport {
                    requested: "http",
                    used: answered.then_some("http"),
                    fallback_reason: None,
                },
                completeness: Some(report.completeness()),
                partial_causes: Vec::new(),
                warnings: warning.iter().cloned().collect(),
                server: ServerNotices::default(),
                error: error.as_ref().map(|(kind, message)| ErrorReport {
                    kind,
                    message: message.clone(),
                }),
                data: report.to_json(),
            };
            Rendered::line(&mut out.stdout, &envelope.to_json().to_string());
        }
        OperatorFormat::Text => {
            Rendered::line(
                &mut out.stdout,
                &format!("packdb support-bundle: {}", display_endpoint(endpoint)),
            );
            out.stdout.push_str(&report.to_text());
        }
    }
    for missing in &report.missing_evidence {
        Rendered::line(
            &mut out.stderr,
            &format!("note: missing {}: {}", missing.evidence, missing.reason),
        );
    }
    if let Some(warning) = warning {
        Rendered::line(&mut out.stderr, &format!("warning: {warning}"));
    }
    if let Some((_, message)) = error {
        Rendered::line(&mut out.stderr, &format!("error: {message}"));
    }
    out
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn checksums_and_names_are_stable() {
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let name = default_output(SystemTime::UNIX_EPOCH);
        assert_eq!(
            name,
            PathBuf::from("packdb-support-bundle-19700101T000000Z.tar.zst")
        );
    }

    #[test]
    fn a_written_bundle_reads_back_and_never_replaces_a_file() {
        let dir = std::env::temp_dir().join(format!("packdb-bundle-unit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let output = dir.join("bundle.tar.zst");
        let _ = std::fs::remove_file(&output);
        let scanner = SecretScanner::new(&packdb_client::AuthConfig::none()).expect("scanner");
        let report = Report::new("support_bundle");
        let files = vec![(
            "server/info.json".to_string(),
            b"{\"name\":\"packdb\"}".to_vec(),
        )];
        let written = write_bundle(&report, &files, &output, &scanner).expect("written");
        let archive = std::fs::read(&output).expect("read");
        assert_eq!(written.sha256, sha256(&archive));
        let entries = untar(&zstd::decode_all(archive.as_slice()).expect("zstd")).expect("tar");
        let names: Vec<&str> = entries.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            ["manifest.json", "checksums.sha256", "server/info.json"]
        );
        let manifest: Value = serde_json::from_slice(&entries[0].1).expect("manifest");
        assert_eq!(manifest["format"], BUNDLE_FORMAT);
        assert_eq!(
            manifest["files"][0]["sha256"],
            sha256(b"{\"name\":\"packdb\"}")
        );
        let again = write_bundle(&report, &files, &output, &scanner);
        assert!(again.is_err(), "an existing file is never replaced");
        assert_eq!(std::fs::read(&output).expect("read"), archive);
        let leftovers = std::fs::read_dir(&dir)
            .expect("list")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".partial"))
            .count();
        assert_eq!(leftovers, 0);
        std::fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn the_tar_is_deterministic_and_reads_back() {
        let entries = vec![
            ("manifest.json".to_string(), b"{}".to_vec()),
            ("server/info.json".to_string(), vec![b'x'; 513]),
            ("empty".to_string(), Vec::new()),
        ];
        let tar = tar_entries(&entries).expect("tar");
        assert_eq!(tar_entries(&entries).expect("again"), tar, "deterministic");
        assert_eq!(untar(&tar).expect("read"), entries);
        let path =
            std::env::temp_dir().join(format!("packdb-bundle-tar-{}.tar", std::process::id()));
        std::fs::write(&path, &tar).expect("write");
        let listed = std::process::Command::new("tar")
            .arg("-tf")
            .arg(&path)
            .output();
        std::fs::remove_file(&path).expect("remove");
        if let Ok(listed) = listed {
            assert!(listed.status.success(), "{listed:?}");
            let names = String::from_utf8(listed.stdout).expect("utf-8");
            assert_eq!(
                names.lines().collect::<Vec<_>>(),
                ["manifest.json", "server/info.json", "empty"]
            );
        }
    }

    #[test]
    fn a_failed_write_never_removes_a_file_it_did_not_create() {
        let dir =
            std::env::temp_dir().join(format!("packdb-bundle-foreign-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let output = dir.join("bundle.tar.zst");
        std::fs::write(&output, b"someone else's bundle").expect("existing output");
        let foreign = dir.join(".packdb-support-bundle-foreign.partial");
        std::fs::write(&foreign, b"someone else's file").expect("foreign file");
        // The predictable name an earlier version wrote to, and removed even
        // when it had not created it.
        let predictable = dir.join(format!(".bundle.tar.zst.partial-{}", std::process::id()));
        std::fs::write(&predictable, b"someone else's file").expect("predictable file");
        let scanner = SecretScanner::new(&packdb_client::AuthConfig::none()).expect("scanner");
        let refused = write_bundle(&Report::new("support_bundle"), &[], &output, &scanner);
        assert!(refused.is_err());
        assert_eq!(
            std::fs::read(&output).expect("output"),
            b"someone else's bundle"
        );
        assert_eq!(
            std::fs::read(&foreign).expect("foreign"),
            b"someone else's file"
        );
        assert_eq!(
            std::fs::read(&predictable).expect("predictable"),
            b"someone else's file"
        );
        let ours = std::fs::read_dir(&dir)
            .expect("list")
            .filter_map(Result::ok)
            .filter(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                name.ends_with(".partial") && !name.contains("foreign")
            })
            .count();
        assert_eq!(ours, 0, "this run's temporary file is gone");
        std::fs::remove_dir_all(&dir).expect("clean up");
    }

    #[test]
    fn a_manifest_that_would_carry_a_secret_writes_nothing() {
        let dir = std::env::temp_dir().join(format!("packdb-bundle-secret-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let output = dir.join("bundle.tar.zst");
        let scanner =
            SecretScanner::new(&packdb_client::AuthConfig::api_key("topsecretvalue".into()))
                .expect("scanner");
        let mut report = Report::new("support_bundle");
        report
            .target
            .insert("endpoint", "http://topsecretvalue@host".into());
        let refused = write_bundle(&report, &[], &output, &scanner);
        assert!(
            matches!(&refused, Err(reason) if reason.contains("configured_credential")),
            "{refused:?}"
        );
        assert!(!output.exists());
        std::fs::remove_dir_all(&dir).expect("clean up");
    }
}
