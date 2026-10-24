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

//! `packdb doctor`: limited diagnostics of a running server.
//!
//! Doctor must not create state on the server it examines. The server's
//! health, readiness, OTLP and query routes all resolve the request's tenant,
//! and several create the tenant's store on first use, so doctor does not call
//! them: their checks are `withheld` and recorded as missing evidence, which
//! keeps the report incomplete until the server offers a diagnostics surface
//! that creates nothing. Doctor reads only `GET /info`, which takes no tenant.
//! Checks the server's HTTP API gives no evidence for at all are `not_exposed`.

use std::time::SystemTime;

use serde_json::Value;

use crate::Io;
use crate::connection::ConnectionArgs;
use crate::exit::Exit;
use crate::output::{
    Envelope, ErrorReport, OperatorFormat, ServerNotices, TransportReport, display_endpoint,
};
use crate::probe::{ProbeError, ProbeResponse, Prober};
use crate::render::{Rendered, deliver};
use crate::report::{Finding, MissingEvidence, Report, Severity, Status};

/// Arguments of `packdb doctor`.
#[derive(Debug, Clone, clap::Args)]
pub struct DoctorArgs {
    /// Where to connect and how to authenticate.
    #[command(flatten)]
    pub connection: ConnectionArgs,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OperatorFormat::Text)]
    pub format: OperatorFormat,
}

/// Required checks doctor does not run, because the routes they read can
/// create tenant state on the server.
const WITHHELD: [(&str, &str); 4] = [
    ("process.health", "process health (/health/detailed)"),
    ("process.ready", "readiness (/ready)"),
    (
        "ingest.otlp_routes_mounted",
        "OTLP routes (/v1/metrics, /v1/logs, /v1/traces)",
    ),
    ("query.surface_availability", "query surfaces"),
];

const WITHHELD_REASON: &str = "withheld: the route resolves the request's tenant and can create \
                               tenant state on the server; doctor checks it once the server \
                               offers a diagnostics route that creates nothing";

/// Checks whose evidence the server's HTTP API does not expose.
const NOT_EXPOSED: [(&str, &str); 9] = [
    (
        "process.build_match",
        "the source revision it was built from",
    ),
    ("durability.ages", "WAL and checkpoint ages"),
    ("durability.recovery_state", "a dedicated recovery state"),
    ("storage.tiers_attached", "which storage tiers are attached"),
    (
        "ingest.rejection_ratio",
        "accepted and rejected record counts",
    ),
    ("ingest.backpressure", "ingest queue depth and backpressure"),
    ("limits.query_pool", "query memory pool use"),
    ("limits.admission", "admission rejections"),
    (
        "limits.cardinality",
        "active series against the cardinality limit",
    ),
];

pub(crate) async fn run(args: DoctorArgs, io: Io<'_>) -> Exit {
    let Io {
        stdout,
        stderr,
        env,
    } = io;
    let connection = match args.connection.resolve(env) {
        Ok(connection) => connection,
        Err(error) => return deliver(stdout, stderr, &Rendered::error(&error.0), Exit::Usage),
    };
    let endpoint = connection.endpoint.clone();
    let tenant_supplied = connection.tenant.is_some();
    let prober = match Prober::new(connection) {
        Ok(prober) => prober,
        Err(error) => return deliver(stdout, stderr, &Rendered::error(&error.0), Exit::Usage),
    };
    let mut doctor = Doctor::new(&endpoint, tenant_supplied);
    doctor.check(&prober).await;
    doctor.report.finished_at = SystemTime::now();
    let out = doctor.render(&endpoint, args.format);
    deliver(stdout, stderr, &out, doctor.exit())
}

/// One doctor run: the report, and the answer that never came.
struct Doctor {
    report: Report,
    failure: Option<ProbeError>,
}

impl Doctor {
    fn new(endpoint: &str, tenant_supplied: bool) -> Doctor {
        let mut report = Report::new("doctor");
        report
            .target
            .insert("endpoint", display_endpoint(endpoint).into());
        report
            .target
            .insert("tenant_supplied", tenant_supplied.into());
        report.scope.insert("diagnostics", "limited".into());
        Doctor {
            report,
            failure: None,
        }
    }

    async fn check(&mut self, prober: &Prober) {
        const ID: &str = "process.version";
        let info = match prober.get("/info").await {
            Ok(response) => read_info(&response),
            Err(error) => {
                let message = error.message().to_string();
                self.failure = Some(error);
                Err(message)
            }
        };
        match info {
            Ok(info) => {
                if let Some(environment) = &info.environment {
                    self.report
                        .target
                        .insert("environment", environment.clone().into());
                }
                self.report.push(
                    Finding::new(
                        ID,
                        Severity::Info,
                        Status::Pass,
                        format!("the server runs {} {}", info.name, info.version),
                    )
                    .with("server_name", info.name)
                    .with("server_version", info.version)
                    .with("client_version", env!("CARGO_PKG_VERSION")),
                );
            }
            Err(reason) => {
                self.report
                    .uncollected(ID, Severity::Required, "server identity", &reason)
            }
        }
        for (id, evidence) in WITHHELD {
            self.report.push(Finding::new(
                id,
                Severity::Required,
                Status::Withheld,
                format!("unknown: {evidence} not checked; {WITHHELD_REASON}"),
            ));
            self.report.missing_evidence.push(MissingEvidence {
                evidence: evidence.to_string(),
                reason: WITHHELD_REASON.to_string(),
            });
        }
        for (id, what) in NOT_EXPOSED {
            self.report.push(Finding::new(
                id,
                Severity::Info,
                Status::NotExposed,
                format!("unknown: the server's HTTP API does not expose {what}"),
            ));
        }
    }

    fn exit(&self) -> Exit {
        match &self.failure {
            Some(failure) => self.report.exit().worst(failure.exit()),
            None => self.report.exit(),
        }
    }

    fn render(&self, endpoint: &str, format: OperatorFormat) -> Rendered {
        let mut out = Rendered::default();
        let withheld = WITHHELD.len();
        let uncollected = self.report.missing_evidence.len() - withheld;
        let warning = if uncollected == 0 {
            format!(
                "completeness unknown (limited diagnostics: {withheld} required check(s) withheld)"
            )
        } else {
            format!(
                "completeness unknown (limited diagnostics: {withheld} required check(s) \
                 withheld, {uncollected} could not be collected)"
            )
        };
        let error = self
            .failure
            .as_ref()
            .filter(|failure| failure.exit() != Exit::Incomplete);
        match format {
            OperatorFormat::Json => {
                let answered = !matches!(self.failure, Some(ProbeError::Unreachable(_)));
                let envelope = Envelope {
                    command: "doctor",
                    endpoint: endpoint.to_string(),
                    transport: TransportReport {
                        requested: "http",
                        used: answered.then_some("http"),
                        fallback_reason: None,
                    },
                    completeness: Some(self.report.completeness()),
                    partial_causes: Vec::new(),
                    warnings: vec![warning.clone()],
                    server: ServerNotices::default(),
                    error: error.map(|failure| ErrorReport {
                        kind: probe_error_kind(failure),
                        message: failure.message().to_string(),
                    }),
                    data: self.report.to_json(),
                };
                Rendered::line(&mut out.stdout, &envelope.to_json().to_string());
            }
            OperatorFormat::Text => {
                let mut header = format!(
                    "packdb doctor (limited diagnostics): {}",
                    display_endpoint(endpoint)
                );
                if let Some(Value::String(environment)) = self.report.target.get("environment") {
                    header.push_str(&format!(" (environment {environment})"));
                }
                Rendered::line(&mut out.stdout, &header);
                out.stdout.push_str(&self.report.to_text());
            }
        }
        for missing in &self.report.missing_evidence {
            if missing.reason != WITHHELD_REASON {
                Rendered::line(
                    &mut out.stderr,
                    &format!("note: missing {}: {}", missing.evidence, missing.reason),
                );
            }
        }
        Rendered::line(&mut out.stderr, &format!("warning: {warning}"));
        if let Some(error) = error {
            Rendered::line(&mut out.stderr, &format!("error: {}", error.message()));
        }
        out
    }
}

fn probe_error_kind(error: &ProbeError) -> &'static str {
    match error {
        ProbeError::Unauthenticated(_) => "unauthenticated",
        ProbeError::PermissionDenied(_) => "permission_denied",
        ProbeError::Unreachable(_) => "unreachable",
        ProbeError::NoEvidence(_) => "server_or_protocol",
    }
}

struct Info {
    name: String,
    version: String,
    environment: Option<String>,
}

/// `/info`: `200` with the service's name and version. A partial answer is
/// not evidence.
fn read_info(response: &ProbeResponse) -> Result<Info, String> {
    if !response.partial_causes.is_empty() {
        return Err(format!(
            "/info answered with a partial result ({})",
            response.partial_causes.join(", ")
        ));
    }
    let body = response.json().unwrap_or(Value::Null);
    let text = |key: &str| body.get(key).and_then(Value::as_str).map(str::to_string);
    match (response.status.as_u16(), text("name"), text("version")) {
        (200, Some(name), Some(version)) => Ok(Info {
            name,
            version,
            environment: text("environment"),
        }),
        (status, _, _) => Err(format!(
            "/info answered {status} without the service information shape"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn response(status: u16, body: Value, causes: &[&str]) -> ProbeResponse {
        ProbeResponse {
            status: http::StatusCode::from_u16(status).unwrap_or(http::StatusCode::IM_A_TEAPOT),
            body: body.to_string().into_bytes(),
            partial_causes: causes.iter().map(|cause| (*cause).to_string()).collect(),
        }
    }

    #[test]
    fn info_needs_a_complete_200_with_name_and_version() {
        let info = json!({"name": "packdb", "version": "1", "environment": "dev"});
        let read = read_info(&response(200, info.clone(), &[]));
        assert!(
            matches!(read, Ok(Info { ref environment, .. }) if environment.as_deref() == Some("dev"))
        );
        assert!(read_info(&response(200, json!({"name": "packdb"}), &[])).is_err());
        assert!(read_info(&response(503, info.clone(), &[])).is_err());
        assert!(read_info(&response(200, info, &["cold_tier_unavailable"])).is_err());
    }
}
