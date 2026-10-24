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

//! `packdb doctor`: check a running server through its HTTP surface.
//!
//! Every verdict comes from an answer the server gave. A check whose answer
//! could not be obtained is an `error` and the report is incomplete; a check
//! the server's HTTP API gives no evidence for is `not_exposed`, listed so the
//! report never implies it was verified. Doctor sends only `GET` requests.

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
use crate::report::{Finding, Report, Severity, Status};

/// A query surface doctor can check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Surface {
    /// Prometheus query API.
    Promql,
    /// Loki query API.
    Logql,
    /// Tempo search API.
    Traceql,
    /// Jaeger query API.
    Jaeger,
    /// SQL over HTTP.
    Sql,
}

impl Surface {
    const ALL: [Surface; 5] = [
        Surface::Promql,
        Surface::Logql,
        Surface::Traceql,
        Surface::Jaeger,
        Surface::Sql,
    ];

    fn name(self) -> &'static str {
        match self {
            Surface::Promql => "promql",
            Surface::Logql => "logql",
            Surface::Traceql => "traceql",
            Surface::Jaeger => "jaeger",
            Surface::Sql => "sql",
        }
    }
}

/// Arguments of `packdb doctor`.
#[derive(Debug, Clone, clap::Args)]
pub struct DoctorArgs {
    /// Where to connect and how to authenticate.
    #[command(flatten)]
    pub connection: ConnectionArgs,

    /// Query surfaces the server is expected to serve [default: all]
    #[arg(
        long = "surface",
        value_enum,
        value_delimiter = ',',
        value_name = "SURFACE"
    )]
    pub surfaces: Vec<Surface>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OperatorFormat::Text)]
    pub format: OperatorFormat,
}

const OTLP_SIGNALS: [(&str, &str); 3] = [
    ("metrics", "/v1/metrics"),
    ("logs", "/v1/logs"),
    ("traces", "/v1/traces"),
];

/// Checks whose evidence the server's HTTP API does not expose, with what it
/// would take.
const NOT_EXPOSED: [(&str, &str); 8] = [
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
    let surfaces = if args.surfaces.is_empty() {
        Surface::ALL.to_vec()
    } else {
        let mut surfaces = Vec::new();
        for surface in args.surfaces {
            if !surfaces.contains(&surface) {
                surfaces.push(surface);
            }
        }
        surfaces
    };
    let mut doctor = Doctor::new(&endpoint, tenant_supplied, &surfaces);
    doctor.check(&prober, &surfaces).await;
    doctor.report.finished_at = SystemTime::now();
    let out = doctor.render(&endpoint, args.format);
    deliver(stdout, stderr, &out, doctor.exit())
}

/// One doctor run: the report, and every answer that never came.
struct Doctor {
    report: Report,
    failures: Vec<ProbeError>,
    answered: bool,
}

impl Doctor {
    fn new(endpoint: &str, tenant_supplied: bool, surfaces: &[Surface]) -> Doctor {
        let mut report = Report::new("doctor");
        report
            .target
            .insert("endpoint", display_endpoint(endpoint).into());
        report
            .target
            .insert("tenant_supplied", tenant_supplied.into());
        report.scope.insert(
            "surfaces",
            surfaces
                .iter()
                .map(|surface| surface.name())
                .collect::<Vec<_>>()
                .into(),
        );
        Doctor {
            report,
            failures: Vec::new(),
            answered: false,
        }
    }

    /// A probe's answer, or `None` with its failure recorded. A partial
    /// answer is not evidence: a projection of the server's state cannot be
    /// built from an incomplete one.
    async fn probe(
        &mut self,
        prober: &Prober,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<ProbeResponse, String> {
        // A server that has answered nothing and let a request go unanswered
        // is not sent the rest: each would wait out the same timeout.
        if !self.answered
            && self
                .failures
                .iter()
                .any(|failure| matches!(failure, ProbeError::Unreachable(_)))
        {
            return Err(format!(
                "{path} was not requested: the server answered no earlier request"
            ));
        }
        match prober.get(path, query).await {
            Ok(response) => {
                self.answered = true;
                if response.partial_causes.is_empty() {
                    Ok(response)
                } else {
                    Err(format!(
                        "{path} answered with a partial result ({})",
                        response.partial_causes.join(", ")
                    ))
                }
            }
            Err(error) => {
                let message = error.message().to_string();
                self.failures.push(error);
                Err(message)
            }
        }
    }

    async fn check(&mut self, prober: &Prober, surfaces: &[Surface]) {
        let info = self.probe(prober, "/info", &[]).await;
        let health = self.probe(prober, "/health/detailed", &[]).await;
        let info = info.and_then(|response| read_info(&response));
        let health = health.and_then(|response| read_health(&response));
        self.health(&health);
        let ready = self.probe(prober, "/ready", &[]).await;
        self.ready(ready.and_then(|response| read_ready(&response)));
        self.version(&info, &health);
        self.report.push(Finding::new(
            "process.build_match",
            Severity::Info,
            Status::NotExposed,
            "unknown: the server exposes its version but no source revision, so which build \
             it runs cannot be verified",
        ));
        self.otlp(prober).await;
        self.surfaces(prober, surfaces).await;
        for (id, what) in NOT_EXPOSED {
            self.report.push(Finding::new(
                id,
                Severity::Info,
                Status::NotExposed,
                format!("unknown: the server's HTTP API does not expose {what}"),
            ));
        }
    }

    fn health(&mut self, health: &Result<Health, String>) {
        const ID: &str = "process.health";
        let health = match health {
            Ok(health) => health,
            Err(reason) => {
                self.report
                    .uncollected(ID, Severity::Required, "process health", reason);
                return;
            }
        };
        let not_healthy: Vec<String> = health
            .checks
            .iter()
            .filter(|(_, status)| status != "healthy")
            .map(|(name, status)| format!("{name}={status}"))
            .collect();
        let (status, summary) = match health.status.as_str() {
            "healthy" => (Status::Pass, "the server reports healthy".to_string()),
            "degraded" => (
                Status::Warn,
                format!("the server reports degraded: {}", not_healthy.join(", ")),
            ),
            _ => (
                Status::Fail,
                format!("the server reports unhealthy: {}", not_healthy.join(", ")),
            ),
        };
        self.report.push(
            Finding::new(ID, Severity::Required, status, summary)
                .with("status", health.status.clone())
                .with("checks_not_healthy", not_healthy),
        );
    }

    fn ready(&mut self, ready: Result<Ready, String>) {
        const ID: &str = "process.ready";
        match ready {
            Ok(ready) => {
                let (status, summary) = if ready.ready {
                    (Status::Pass, "the server reports ready".to_string())
                } else {
                    (
                        Status::Fail,
                        format!("the server is not ready: {}", ready.not_ready.join(", ")),
                    )
                };
                self.report.push(
                    Finding::new(ID, Severity::Required, status, summary)
                        .with("ready", ready.ready)
                        .with("checks_not_ready", ready.not_ready),
                );
            }
            Err(reason) => self
                .report
                .uncollected(ID, Severity::Required, "readiness", &reason),
        }
    }

    fn version(&mut self, info: &Result<Info, String>, health: &Result<Health, String>) {
        const ID: &str = "process.version";
        let info = match info {
            Ok(info) => info,
            Err(reason) => {
                self.report
                    .uncollected(ID, Severity::Info, "server identity", reason);
                return;
            }
        };
        if let Some(environment) = &info.environment {
            self.report
                .target
                .insert("environment", environment.clone().into());
        }
        let finding = match health {
            Ok(health) if health.version != info.version => {
                self.report.uncollected(
                    ID,
                    Severity::Info,
                    "server version",
                    &format!(
                        "/info reports {} but /health/detailed reports {}",
                        info.version, health.version
                    ),
                );
                return;
            }
            _ => Finding::new(
                ID,
                Severity::Info,
                Status::Pass,
                format!("the server runs {} {}", info.name, info.version),
            ),
        };
        self.report.push(
            finding
                .with("server_name", info.name.clone())
                .with("server_version", info.version.clone())
                .with("client_version", env!("CARGO_PKG_VERSION")),
        );
    }

    /// Each OTLP endpoint is mounted: `GET` answers `405` naming `POST`.
    async fn otlp(&mut self, prober: &Prober) {
        const ID: &str = "ingest.signals_enabled";
        let mut mounted = Vec::new();
        let mut unproven = Vec::new();
        for (signal, path) in OTLP_SIGNALS {
            let proof = self.probe(prober, path, &[]).await.and_then(|response| {
                let allows_post = response
                    .headers
                    .get(http::header::ALLOW)
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(|value| value.split(',').any(|method| method.trim() == "POST"));
                if response.status == http::StatusCode::METHOD_NOT_ALLOWED && allows_post {
                    Ok(())
                } else {
                    Err(format!(
                        "{path} answered {} instead of 405 with Allow: POST",
                        response.status.as_u16()
                    ))
                }
            });
            match proof {
                Ok(()) => mounted.push(signal),
                Err(reason) => unproven.push((signal, reason)),
            }
        }
        self.graded_list(
            ID,
            "OTLP endpoint",
            &mounted,
            unproven,
            "metrics, logs and traces OTLP endpoints are mounted",
        );
    }

    async fn surfaces(&mut self, prober: &Prober, surfaces: &[Surface]) {
        const ID: &str = "query.surface_availability";
        let mut available = Vec::new();
        let mut unproven = Vec::new();
        for surface in surfaces {
            let (path, query) = surface_probe(*surface);
            let proof = self
                .probe(prober, path, query)
                .await
                .and_then(|response| prove_surface(*surface, path, &response));
            match proof {
                Ok(()) => available.push(surface.name()),
                Err(reason) => unproven.push((surface.name(), reason)),
            }
        }
        self.graded_list(
            ID,
            "query surface",
            &available,
            unproven,
            &format!("{} query surface(s) answer", available.len()),
        );
    }

    /// A required finding over several items: a pass when every item was
    /// proven, otherwise an `error` naming each one whose proof could not be
    /// obtained.
    fn graded_list(
        &mut self,
        id: &'static str,
        noun: &str,
        proven: &[&str],
        unproven: Vec<(&str, String)>,
        pass_summary: &str,
    ) {
        if unproven.is_empty() {
            self.report.push(
                Finding::new(id, Severity::Required, Status::Pass, pass_summary)
                    .with("proven", proven.to_vec()),
            );
            return;
        }
        let names: Vec<&str> = unproven.iter().map(|(name, _)| *name).collect();
        self.report.push(
            Finding::new(
                id,
                Severity::Required,
                Status::Error,
                format!("unknown for {}: no proof was obtained", names.join(", ")),
            )
            .with("proven", proven.to_vec())
            .with("unproven", names),
        );
        for (name, reason) in unproven {
            self.report
                .missing_evidence
                .push(crate::report::MissingEvidence {
                    evidence: format!("{noun} {name}"),
                    reason,
                });
        }
    }

    fn exit(&self) -> Exit {
        let mut exit = self.report.exit();
        for failure in &self.failures {
            match failure {
                ProbeError::Unreachable(_) if self.answered => {}
                failure => exit = exit.worst(failure.exit()),
            }
        }
        exit
    }

    fn render(&self, endpoint: &str, format: OperatorFormat) -> Rendered {
        let mut out = Rendered::default();
        let complete = self.report.is_complete();
        let warning = (!complete).then(|| {
            format!(
                "completeness unknown ({} piece(s) of required evidence could not be collected)",
                self.report.missing_evidence.len().max(1)
            )
        });
        let error = self
            .failures
            .iter()
            .find(|failure| failure.exit() != Exit::Incomplete && self.exit() == failure.exit());
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
                    warnings: warning.iter().cloned().collect(),
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
                let mut header = format!("packdb doctor: {}", display_endpoint(endpoint));
                if let Some(Value::String(environment)) = self.report.target.get("environment") {
                    header.push_str(&format!(" (environment {environment})"));
                }
                Rendered::line(&mut out.stdout, &header);
                out.stdout.push_str(&self.report.to_text());
            }
        }
        for missing in &self.report.missing_evidence {
            Rendered::line(
                &mut out.stderr,
                &format!("note: missing {}: {}", missing.evidence, missing.reason),
            );
        }
        if let Some(warning) = warning {
            Rendered::line(&mut out.stderr, &format!("warning: {warning}"));
        }
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

/// The request that proves a surface's handler ran. PromQL and TraceQL are
/// sent a malformed query and must answer with their typed error; the others
/// must answer with their documented success shape.
fn surface_probe(surface: Surface) -> (&'static str, &'static [(&'static str, &'static str)]) {
    match surface {
        Surface::Promql => ("/api/v1/query", &[("query", "((")]),
        Surface::Logql => ("/loki/api/v1/labels", &[]),
        Surface::Traceql => ("/api/search", &[("q", "{")]),
        Surface::Jaeger => ("/jaeger/api/services", &[]),
        Surface::Sql => ("/api/v1/sql", &[("q", "SELECT 1 AS probe")]),
    }
}

fn prove_surface(surface: Surface, path: &str, response: &ProbeResponse) -> Result<(), String> {
    let body = response.json().unwrap_or(Value::Null);
    let text = |key: &str| body.get(key).and_then(Value::as_str);
    let list = |key: &str| body.get(key).and_then(Value::as_array).is_some();
    let proven = match surface {
        Surface::Promql => {
            response.status.is_client_error()
                && text("status") == Some("error")
                && text("errorType").is_some()
                && text("error").is_some()
        }
        Surface::Traceql => {
            response.status.is_client_error() && text("error").is_some_and(|e| !e.is_empty())
        }
        Surface::Logql => {
            response.status.is_success() && text("status") == Some("success") && list("data")
        }
        Surface::Jaeger => response.status.is_success() && list("data"),
        Surface::Sql => response.status.is_success() && list("schema") && list("rows"),
    };
    if proven {
        Ok(())
    } else {
        Err(format!(
            "{path} answered {} without the {} response shape",
            response.status.as_u16(),
            surface.name()
        ))
    }
}

struct Info {
    name: String,
    version: String,
    environment: Option<String>,
}

fn read_info(response: &ProbeResponse) -> Result<Info, String> {
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

struct Health {
    status: String,
    version: String,
    checks: Vec<(String, String)>,
}

/// `/health/detailed`: `200` with `healthy` or `degraded`, `503` with
/// `unhealthy`, and the documented fields.
fn read_health(response: &ProbeResponse) -> Result<Health, String> {
    let body = response.json().unwrap_or(Value::Null);
    let text = |key: &str| body.get(key).and_then(Value::as_str).map(str::to_string);
    let checks: Option<Vec<(String, String)>> = body
        .get("checks")
        .and_then(Value::as_array)
        .and_then(|checks| {
            checks
                .iter()
                .map(|check| {
                    let name = check.get("name").and_then(Value::as_str)?;
                    let status = check.get("status").and_then(Value::as_str)?;
                    Some((name.to_string(), status.to_string()))
                })
                .collect()
        });
    let shaped =
        text("timestamp").is_some() && body.get("uptime_seconds").and_then(Value::as_u64).is_some();
    let (Some(status), Some(version), Some(checks), true) =
        (text("status"), text("version"), checks, shaped)
    else {
        return Err(format!(
            "/health/detailed answered {} without the detailed health shape",
            response.status.as_u16()
        ));
    };
    let agrees = match response.status.as_u16() {
        200 => matches!(status.as_str(), "healthy" | "degraded"),
        503 => status == "unhealthy",
        _ => false,
    };
    if !agrees {
        return Err(format!(
            "/health/detailed answered {} with status {status}",
            response.status.as_u16()
        ));
    }
    Ok(Health {
        status,
        version,
        checks,
    })
}

struct Ready {
    ready: bool,
    not_ready: Vec<String>,
}

/// `/ready`: `200` with `ready: true` or `503` with `ready: false`.
fn read_ready(response: &ProbeResponse) -> Result<Ready, String> {
    let body = response.json().unwrap_or(Value::Null);
    let checks: Option<Vec<(String, bool)>> = body
        .get("checks")
        .and_then(Value::as_array)
        .and_then(|checks| {
            checks
                .iter()
                .map(|check| {
                    let name = check.get("name").and_then(Value::as_str)?;
                    let ready = check.get("ready").and_then(Value::as_bool)?;
                    Some((name.to_string(), ready))
                })
                .collect()
        });
    let timestamp = body.get("timestamp").and_then(Value::as_str).is_some();
    let (Some(ready), Some(checks), true) = (
        body.get("ready").and_then(Value::as_bool),
        checks,
        timestamp,
    ) else {
        return Err(format!(
            "/ready answered {} without the readiness shape",
            response.status.as_u16()
        ));
    };
    match (response.status.as_u16(), ready) {
        (200, true) | (503, false) => Ok(Ready {
            ready,
            not_ready: checks
                .into_iter()
                .filter(|(_, ready)| !ready)
                .map(|(name, _)| name)
                .collect(),
        }),
        (status, _) => Err(format!("/ready answered {status} with ready={ready}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn response(status: u16, body: Value) -> ProbeResponse {
        ProbeResponse {
            status: http::StatusCode::from_u16(status).unwrap_or(http::StatusCode::IM_A_TEAPOT),
            headers: http::HeaderMap::new(),
            body: body.to_string().into_bytes(),
            partial_causes: Vec::new(),
        }
    }

    #[test]
    fn health_status_must_agree_with_the_http_status() {
        let body = |status: &str| {
            json!({"status": status, "timestamp": "t", "version": "1", "uptime_seconds": 1,
                "checks": [{"name": "storage", "status": status}]})
        };
        assert!(read_health(&response(200, body("healthy"))).is_ok());
        assert!(read_health(&response(200, body("degraded"))).is_ok());
        assert!(read_health(&response(503, body("unhealthy"))).is_ok());
        assert!(read_health(&response(200, body("unhealthy"))).is_err());
        assert!(read_health(&response(503, body("healthy"))).is_err());
        assert!(read_health(&response(200, json!({"status": "healthy"}))).is_err());
    }

    #[test]
    fn readiness_must_agree_with_the_http_status() {
        let body = |ready: bool| {
            json!({"ready": ready, "timestamp": "t",
                "checks": [{"name": "storage", "ready": ready}]})
        };
        assert!(read_ready(&response(200, body(true))).is_ok());
        let not_ready = read_ready(&response(503, body(false)));
        assert!(
            matches!(not_ready, Ok(Ready { ready: false, ref not_ready }) if not_ready == &["storage"])
        );
        assert!(read_ready(&response(200, body(false))).is_err());
        assert!(read_ready(&response(404, json!({}))).is_err());
    }

    #[test]
    fn each_surface_needs_its_own_shape() {
        let typed = json!({"status": "error", "errorType": "bad_data", "error": "parse"});
        assert!(prove_surface(Surface::Promql, "p", &response(400, typed.clone())).is_ok());
        assert!(prove_surface(Surface::Promql, "p", &response(200, typed)).is_err());
        assert!(
            prove_surface(
                Surface::Traceql,
                "t",
                &response(400, json!({"error": "bad"}))
            )
            .is_ok()
        );
        assert!(
            prove_surface(Surface::Traceql, "t", &response(400, json!({"error": ""}))).is_err()
        );
        let labels = json!({"status": "success", "data": []});
        assert!(prove_surface(Surface::Logql, "l", &response(200, labels)).is_ok());
        assert!(prove_surface(Surface::Jaeger, "j", &response(200, json!({"data": []}))).is_ok());
        assert!(prove_surface(Surface::Jaeger, "j", &response(404, json!({"data": []}))).is_err());
        let sql = json!({"schema": [], "rows": []});
        assert!(prove_surface(Surface::Sql, "s", &response(200, sql)).is_ok());
        assert!(prove_surface(Surface::Sql, "s", &response(200, json!({"rows": []}))).is_err());
    }
}
