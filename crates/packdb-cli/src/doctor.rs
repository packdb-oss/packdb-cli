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
//! `/health/detailed`, `/ready`, OTLP and query routes all resolve the
//! request's tenant, and several create the tenant's store on first use, so
//! doctor does not call them. It reads `GET /info` and `GET /health/diagnostics`,
//! the server's read-only health and readiness. Health and readiness there
//! describe the server's own monitoring scope, not the tenant in the request;
//! the route list there is registration, not proof that a route works. Checks
//! doctor cannot run without the creating routes are `withheld` and recorded
//! as missing evidence, which keeps the report incomplete. Checks the server's
//! HTTP API gives no evidence for at all are `not_exposed`.

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

/// Required checks doctor does not run, because only routes that can create
/// tenant state would show them working.
const WITHHELD: [(&str, &str); 2] = [
    (
        "ingest.otlp_routes_mounted",
        "OTLP routes (/v1/metrics, /v1/logs, /v1/traces)",
    ),
    ("query.surface_availability", "query surfaces"),
];

const WITHHELD_REASON: &str = "withheld: the route resolves the request's tenant and can create \
                               tenant state on the server; doctor checks it once the server \
                               offers a diagnostics route that creates nothing";

/// Why health and readiness are withheld from a server without
/// `/health/diagnostics`.
const NO_DIAGNOSTICS_REASON: &str = "withheld: the server offers no /health/diagnostics (404), \
                                     and its /health/detailed and /ready routes can create \
                                     tenant state";

/// The schema doctor reads from `/health/diagnostics`.
const DIAGNOSTICS_SCHEMA: &str = "packdb.server.diagnostics.v1";

const SCOPE_NOTE: &str = "the server's own monitoring scope, not the tenant in the request";

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
    let doctor = diagnose(&endpoint, tenant_supplied, &prober).await;
    let out = doctor.render(&endpoint, args.format);
    deliver(stdout, stderr, &out, doctor.exit())
}

/// Run every doctor check against `prober`'s server.
pub(crate) async fn diagnose(endpoint: &str, tenant_supplied: bool, prober: &Prober) -> Doctor {
    let mut doctor = Doctor::new(endpoint, tenant_supplied);
    doctor.check(prober).await;
    doctor.answered = prober.responded();
    doctor.report.finished_at = SystemTime::now();
    doctor
}

/// One doctor run: the report, the answers that never came, and the server's
/// own answers where they were usable evidence.
pub(crate) struct Doctor {
    pub(crate) report: Report,
    pub(crate) failures: Vec<ProbeError>,
    /// Whether the server sent any response, usable as evidence or not.
    pub(crate) answered: bool,
    /// The `/info` body, when it had the documented shape.
    pub(crate) info_body: Option<Vec<u8>>,
    /// The `/health/diagnostics` body, when it had the documented shape.
    pub(crate) diagnostics_body: Option<Vec<u8>>,
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
            failures: Vec::new(),
            answered: false,
            info_body: None,
            diagnostics_body: None,
        }
    }

    async fn check(&mut self, prober: &Prober) {
        const ID: &str = "process.version";
        let info = self.get(prober, "/info").await.and_then(|response| {
            let info = read_info(&response)?;
            self.info_body = Some(response.body);
            Ok(info)
        });
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
        self.diagnostics(prober).await;
        for (id, evidence) in WITHHELD {
            self.withhold(id, evidence, WITHHELD_REASON);
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

    /// Health, readiness and registered routes from `/health/diagnostics`.
    async fn diagnostics(&mut self, prober: &Prober) {
        let read = if !prober.responded() && self.unreachable() {
            Err("/health/diagnostics was not requested: the server did not answer /info".into())
        } else {
            match self.get(prober, "/health/diagnostics").await {
                Ok(response)
                    if response.status == http::StatusCode::NOT_FOUND
                        && response.partial_causes.is_empty() =>
                {
                    for (id, evidence) in [
                        ("process.health", "process health"),
                        ("process.ready", "readiness"),
                    ] {
                        self.withhold(id, evidence, NO_DIAGNOSTICS_REASON);
                    }
                    return;
                }
                Ok(response) => {
                    let diagnostics = read_diagnostics(&response);
                    if diagnostics.is_ok() {
                        self.diagnostics_body = Some(response.body);
                    }
                    diagnostics
                }
                Err(message) => Err(message),
            }
        };
        let diagnostics = match read {
            Ok(diagnostics) => diagnostics,
            Err(reason) => {
                self.report.uncollected(
                    "process.health",
                    Severity::Required,
                    "process health",
                    &reason,
                );
                self.report
                    .uncollected("process.ready", Severity::Required, "readiness", &reason);
                return;
            }
        };
        let not_healthy: Vec<String> = diagnostics
            .health_checks
            .iter()
            .filter(|(_, status)| status != "healthy")
            .map(|(name, status)| format!("{name}={status}"))
            .collect();
        let (status, verdict) = match diagnostics.health.as_str() {
            "healthy" => (Status::Pass, "healthy".to_string()),
            "degraded" => (
                Status::Warn,
                format!("degraded: {}", not_healthy.join(", ")),
            ),
            _ => (
                Status::Fail,
                format!("unhealthy: {}", not_healthy.join(", ")),
            ),
        };
        self.report.push(
            Finding::new(
                "process.health",
                Severity::Required,
                status,
                format!("the server reports {verdict} ({SCOPE_NOTE})"),
            )
            .with("status", diagnostics.health.clone())
            .with("checks_not_healthy", not_healthy)
            .with("scope", "server"),
        );
        let (status, verdict) = if diagnostics.ready {
            (Status::Pass, "ready".to_string())
        } else {
            (
                Status::Fail,
                format!("not ready: {}", diagnostics.not_ready.join(", ")),
            )
        };
        self.report.push(
            Finding::new(
                "process.ready",
                Severity::Required,
                status,
                format!("the server reports {verdict} ({SCOPE_NOTE})"),
            )
            .with("ready", diagnostics.ready)
            .with("checks_not_ready", diagnostics.not_ready)
            .with("scope", "server"),
        );
        self.report.push(
            Finding::new(
                "routes.registered",
                Severity::Info,
                Status::Pass,
                format!(
                    "the server reports OTLP routes for {} and query routes for {}; \
                     registration does not show that ingestion or queries work",
                    list_or_none(&diagnostics.otlp),
                    list_or_none(&diagnostics.query),
                ),
            )
            .with("otlp", diagnostics.otlp)
            .with("query", diagnostics.query),
        );
    }

    fn withhold(&mut self, id: &'static str, evidence: &str, reason: &str) {
        self.report.push(Finding::new(
            id,
            Severity::Required,
            Status::Withheld,
            format!("unknown: {evidence} not checked; {reason}"),
        ));
        self.report.missing_evidence.push(MissingEvidence {
            evidence: evidence.to_string(),
            reason: reason.to_string(),
        });
    }

    /// `GET path`: the answer, or the failure recorded and its message.
    async fn get(&mut self, prober: &Prober, path: &str) -> Result<ProbeResponse, String> {
        prober.get(path).await.map_err(|error| {
            let message = error.message().to_string();
            self.failures.push(error);
            message
        })
    }

    fn unreachable(&self) -> bool {
        self.failures
            .iter()
            .any(|failure| matches!(failure, ProbeError::Unreachable(_)))
    }

    /// A server that answered any request was reached: a later request it
    /// did not answer is missing evidence, not an unreachable server.
    pub(crate) fn exit(&self) -> Exit {
        self.failures
            .iter()
            .filter(|failure| !(self.answered && matches!(failure, ProbeError::Unreachable(_))))
            .fold(self.report.exit(), |exit, failure| {
                exit.worst(failure.exit())
            })
    }

    fn render(&self, endpoint: &str, format: OperatorFormat) -> Rendered {
        let mut out = Rendered::default();
        let count = |wanted: Status| {
            self.report
                .findings
                .iter()
                .filter(|finding| finding.status == wanted)
                .count()
        };
        let withheld = count(Status::Withheld);
        let uncollected = count(Status::Error);
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
        let exit = self.exit();
        let error = self
            .failures
            .iter()
            .find(|failure| failure.exit() != Exit::Incomplete && failure.exit() == exit);
        match format {
            OperatorFormat::Json => {
                let envelope = Envelope {
                    command: "doctor",
                    endpoint: endpoint.to_string(),
                    transport: TransportReport {
                        requested: "http",
                        used: self.answered.then_some("http"),
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
            if !missing.reason.starts_with("withheld:") {
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

fn list_or_none(items: &[String]) -> String {
    if items.is_empty() {
        "none".to_string()
    } else {
        items.join(", ")
    }
}

struct Diagnostics {
    health: String,
    health_checks: Vec<(String, String)>,
    ready: bool,
    not_ready: Vec<String>,
    otlp: Vec<String>,
    query: Vec<String>,
}

/// `/health/diagnostics`: a complete `200` with the documented schema.
fn read_diagnostics(response: &ProbeResponse) -> Result<Diagnostics, String> {
    let malformed = || {
        format!(
            "/health/diagnostics answered {} without the {DIAGNOSTICS_SCHEMA} shape",
            response.status.as_u16()
        )
    };
    if !response.partial_causes.is_empty() {
        return Err(format!(
            "/health/diagnostics answered with a partial result ({})",
            response.partial_causes.join(", ")
        ));
    }
    if response.status != http::StatusCode::OK {
        return Err(malformed());
    }
    let body = response.json().ok_or_else(malformed)?;
    if body.get("schema_version").and_then(Value::as_str) != Some(DIAGNOSTICS_SCHEMA) {
        return Err(malformed());
    }
    let pairs = |value: Option<&Value>, key: &str| -> Option<Vec<(String, Value)>> {
        value?
            .as_array()?
            .iter()
            .map(|item| {
                Some((
                    item.get("name")?.as_str()?.to_string(),
                    item.get(key)?.clone(),
                ))
            })
            .collect()
    };
    let strings = |value: Option<&Value>| -> Option<Vec<String>> {
        value?
            .as_array()?
            .iter()
            .map(|item| item.as_str().map(str::to_string))
            .collect()
    };
    let health = body.get("health");
    let readiness = body.get("readiness");
    let routes = body.get("routes");
    let status = health
        .and_then(|health| health.get("status"))
        .and_then(Value::as_str)
        .filter(|status| matches!(*status, "healthy" | "degraded" | "unhealthy"));
    let health_checks =
        pairs(health.and_then(|health| health.get("checks")), "status").and_then(|checks| {
            checks
                .into_iter()
                .map(|(name, status)| Some((name, status.as_str()?.to_string())))
                .collect::<Option<Vec<_>>>()
        });
    let ready = readiness
        .and_then(|readiness| readiness.get("ready"))
        .and_then(Value::as_bool);
    let ready_checks = pairs(
        readiness.and_then(|readiness| readiness.get("checks")),
        "ready",
    )
    .and_then(|checks| {
        checks
            .into_iter()
            .map(|(name, ready)| Some((name, ready.as_bool()?)))
            .collect::<Option<Vec<_>>>()
    });
    let otlp = strings(routes.and_then(|routes| routes.get("otlp")));
    let query = strings(routes.and_then(|routes| routes.get("query")));
    match (status, health_checks, ready, ready_checks, otlp, query) {
        (
            Some(status),
            Some(health_checks),
            Some(ready),
            Some(ready_checks),
            Some(otlp),
            Some(query),
        ) => Ok(Diagnostics {
            health: status.to_string(),
            health_checks,
            ready,
            not_ready: ready_checks
                .into_iter()
                .filter(|(_, ready)| !ready)
                .map(|(name, _)| name)
                .collect(),
            otlp,
            query,
        }),
        _ => Err(malformed()),
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
            content_type: Some("application/json".into()),
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
