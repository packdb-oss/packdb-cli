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

//! `packdb usage-report`: a running server's resource usage.
//!
//! The report reads `GET /health/usage`, which creates no state on the server.
//! Each measurement there names its scope (the whole process, or one tenant)
//! and its period (current, or accumulated since the process started), and the
//! report keeps both. A value the server reports as unavailable is missing
//! evidence, never a zero: the report is then incomplete. What the server
//! does not measure at all — ingest per signal or per tenant, totals over a
//! chosen window, query counts, limits — is `not_exposed`.

use std::time::SystemTime;

use serde_json::{Map, Value};

use crate::Io;
use crate::connection::ConnectionArgs;
use crate::doctor::probe_error_kind;
use crate::exit::Exit;
use crate::output::{
    Envelope, ErrorReport, OperatorFormat, ServerNotices, TransportReport, display_endpoint,
};
use crate::probe::{ProbeError, ProbeResponse, Prober};
use crate::render::{Rendered, deliver};
use crate::report::{Finding, MissingEvidence, Report, Severity, Status};

/// Arguments of `packdb usage-report`.
#[derive(Debug, Clone, clap::Args)]
pub struct UsageReportArgs {
    /// Where to connect and how to authenticate.
    #[command(flatten)]
    pub connection: ConnectionArgs,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OperatorFormat::Text)]
    pub format: OperatorFormat,
}

/// The schema the report reads from `/health/usage`.
const USAGE_SCHEMA: &str = "packdb.server.usage.v1";

/// One measurement of `/health/usage`: the finding it becomes, its key in the
/// body, the scope and period it must declare, and the fields of its value.
struct Measurement {
    id: &'static str,
    key: &'static str,
    evidence: &'static str,
    scope: &'static str,
    period: &'static str,
    fields: &'static [&'static str],
}

const MEASUREMENTS: [Measurement; 3] = [
    Measurement {
        id: "usage.memory",
        key: "memory",
        evidence: "tier memory",
        scope: "process",
        period: "current",
        fields: &[
            "resident_bytes",
            "budget_bytes",
            "hot_resident_bytes",
            "warm_resident_bytes",
            "cold_resident_bytes",
            "external_bytes",
        ],
    },
    Measurement {
        id: "usage.ingest",
        key: "ingest",
        evidence: "accepted ingest",
        scope: "process",
        period: "since_process_start",
        fields: &["accepted_points"],
    },
    Measurement {
        id: "usage.storage",
        key: "storage",
        evidence: "tenant storage",
        scope: "tenant",
        period: "current",
        fields: &["hot_metric_names", "hot_metric_rows"],
    },
];

/// Usage the server's HTTP API does not measure.
const NOT_EXPOSED: [(&str, &str); 5] = [
    ("usage.ingest_by_signal", "accepted ingest per signal"),
    ("usage.ingest_by_tenant", "accepted ingest per tenant"),
    ("usage.windowed_totals", "usage over a chosen time window"),
    ("usage.queries", "query counts"),
    ("usage.limits", "configured limits or their utilization"),
];

/// Why the measurements are withheld from a server without `/health/usage`.
const NO_USAGE_REASON: &str = "withheld: the server offers no /health/usage (404), and its \
                               other usage routes can create tenant state";

pub(crate) async fn run(args: UsageReportArgs, io: Io<'_>) -> Exit {
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
    let prober = match Prober::new(connection) {
        Ok(prober) => prober,
        Err(error) => return deliver(stdout, stderr, &Rendered::error(&error.0), Exit::Usage),
    };
    let mut usage = UsageReport::new(&endpoint);
    usage.collect(&prober).await;
    usage.answered = prober.responded();
    usage.report.finished_at = SystemTime::now();
    let out = usage.render(&endpoint, args.format);
    deliver(stdout, stderr, &out, usage.exit())
}

struct UsageReport {
    report: Report,
    failure: Option<ProbeError>,
    answered: bool,
}

impl UsageReport {
    fn new(endpoint: &str) -> UsageReport {
        let mut report = Report::new("usage-report");
        report
            .target
            .insert("endpoint", display_endpoint(endpoint).into());
        UsageReport {
            report,
            failure: None,
            answered: false,
        }
    }

    async fn collect(&mut self, prober: &Prober) {
        let read = match prober.get("/health/usage").await {
            Ok(response)
                if response.status == http::StatusCode::NOT_FOUND
                    && response.partial_causes.is_empty() =>
            {
                for measurement in &MEASUREMENTS {
                    self.withhold(measurement);
                }
                self.not_exposed();
                return;
            }
            Ok(response) => read_usage(&response),
            Err(error) => {
                let message = error.message().to_string();
                self.failure = Some(error);
                Err(message)
            }
        };
        match read {
            Ok(body) => {
                if let Some(observed_at) = body.get("observed_at").and_then(Value::as_str) {
                    self.report
                        .target
                        .insert("server_observed_at", observed_at.into());
                }
                for measurement in &MEASUREMENTS {
                    self.measure(measurement, &body);
                }
            }
            Err(reason) => {
                for measurement in &MEASUREMENTS {
                    self.report.uncollected(
                        measurement.id,
                        Severity::Required,
                        measurement.evidence,
                        &reason,
                    );
                }
            }
        }
        self.not_exposed();
    }

    /// One measurement already checked by [`read_usage`]: its value, or the
    /// server's reason it has none.
    fn measure(&mut self, measurement: &Measurement, body: &Value) {
        let entry = body.get(measurement.key);
        let value = entry.and_then(|entry| entry.get("value"));
        let Some(value) = value.and_then(Value::as_object) else {
            let reason = entry
                .and_then(|entry| entry.get("unavailable_reason"))
                .and_then(Value::as_str)
                .unwrap_or("no reason given");
            self.report.uncollected(
                measurement.id,
                Severity::Required,
                measurement.evidence,
                &format!("the server reports it unavailable: {reason}"),
            );
            return;
        };
        let number = |key: &str| value.get(key).and_then(Value::as_u64).unwrap_or(0);
        let tenant = entry
            .and_then(|entry| entry.get("tenant_id"))
            .and_then(Value::as_u64);
        let summary = match measurement.key {
            "memory" => format!(
                "{} bytes resident in the storage tiers of a {}-byte budget, {} bytes outside \
                 them (process-wide, current)",
                number("resident_bytes"),
                number("budget_bytes"),
                number("external_bytes"),
            ),
            "ingest" => format!(
                "{} points accepted across all signals since the process started (process-wide)",
                number("accepted_points"),
            ),
            _ => format!(
                "tenant {}: {} bytes stored across {} tier(s); {} metric rows under {} names in \
                 the hot tier (current)",
                tenant.map_or_else(|| "?".to_string(), |tenant| tenant.to_string()),
                stored_bytes(value),
                value
                    .get("tiers")
                    .and_then(Value::as_object)
                    .map_or(0, Map::len),
                number("hot_metric_rows"),
                number("hot_metric_names"),
            ),
        };
        let mut finding = Finding::new(measurement.id, Severity::Required, Status::Pass, summary)
            .with("scope", measurement.scope)
            .with("period", measurement.period)
            .with("value", Value::Object(value.clone()));
        if let Some(tenant) = tenant {
            finding = finding.with("tenant_id", tenant);
        }
        self.report.push(finding);
    }

    fn withhold(&mut self, measurement: &Measurement) {
        self.report.push(Finding::new(
            measurement.id,
            Severity::Required,
            Status::Withheld,
            format!(
                "unknown: {} not read; {NO_USAGE_REASON}",
                measurement.evidence
            ),
        ));
        self.report.missing_evidence.push(MissingEvidence {
            evidence: measurement.evidence.to_string(),
            reason: NO_USAGE_REASON.to_string(),
        });
    }

    fn not_exposed(&mut self) {
        for (id, what) in NOT_EXPOSED {
            self.report.push(Finding::new(
                id,
                Severity::Info,
                Status::NotExposed,
                format!("unknown: the server's HTTP API does not expose {what}"),
            ));
        }
    }

    /// A failed request's exit, unless the server answered it: an answer that
    /// was not usable is missing evidence, already in the report.
    fn exit(&self) -> Exit {
        let exit = self.report.exit();
        match &self.failure {
            Some(failure) if !(self.answered && matches!(failure, ProbeError::Unreachable(_))) => {
                exit.worst(failure.exit())
            }
            _ => exit,
        }
    }

    fn render(&self, endpoint: &str, format: OperatorFormat) -> Rendered {
        let mut out = Rendered::default();
        let unknown = self
            .report
            .findings
            .iter()
            .filter(|finding| matches!(finding.status, Status::Error | Status::Withheld))
            .count();
        let warning = (unknown > 0)
            .then(|| format!("completeness unknown: {unknown} measurement(s) not available"));
        let exit = self.exit();
        let error = self
            .failure
            .as_ref()
            .filter(|failure| failure.exit() != Exit::Incomplete && failure.exit() == exit);
        match format {
            OperatorFormat::Json => {
                let envelope = Envelope {
                    command: "usage-report",
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
                Rendered::line(
                    &mut out.stdout,
                    &format!("packdb usage-report: {}", display_endpoint(endpoint)),
                );
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
        if let Some(warning) = warning {
            Rendered::line(&mut out.stderr, &format!("warning: {warning}"));
        }
        if let Some(error) = error {
            Rendered::line(&mut out.stderr, &format!("error: {}", error.message()));
        }
        out
    }
}

/// Stored bytes summed over the tenant's tiers.
fn stored_bytes(value: &Map<String, Value>) -> u64 {
    value
        .get("tiers")
        .and_then(Value::as_object)
        .map_or(0, |tiers| {
            tiers
                .values()
                .filter_map(|tier| tier.get("stored_bytes").and_then(Value::as_u64))
                .fold(0u64, u64::saturating_add)
        })
}

/// `/health/usage`: a complete `200` with the documented schema, every
/// measurement declaring the scope and period this report expects, and a
/// value that is either `null` with a reason or carries every field.
fn read_usage(response: &ProbeResponse) -> Result<Value, String> {
    if !response.partial_causes.is_empty() {
        return Err(format!(
            "/health/usage answered with a partial result ({})",
            response.partial_causes.join(", ")
        ));
    }
    let malformed = |what: &str| {
        format!(
            "/health/usage answered {} without the {USAGE_SCHEMA} shape ({what})",
            response.status.as_u16()
        )
    };
    if response.status != http::StatusCode::OK {
        return Err(malformed("not 200"));
    }
    let body = response.json().ok_or_else(|| malformed("not JSON"))?;
    if body.get("schema_version").and_then(Value::as_str) != Some(USAGE_SCHEMA) {
        return Err(malformed("schema_version"));
    }
    for measurement in &MEASUREMENTS {
        let entry = body
            .get(measurement.key)
            .ok_or_else(|| malformed(measurement.key))?;
        let text = |key: &str| entry.get(key).and_then(Value::as_str);
        if text("scope") != Some(measurement.scope) || text("period") != Some(measurement.period) {
            return Err(malformed(&format!("{} scope or period", measurement.key)));
        }
        if measurement.scope == "tenant" && entry.get("tenant_id").and_then(Value::as_u64).is_none()
        {
            return Err(malformed(&format!("{} tenant_id", measurement.key)));
        }
        match entry.get("value") {
            Some(Value::Null) if text("unavailable_reason").is_some() => {}
            Some(Value::Object(value))
                if measurement
                    .fields
                    .iter()
                    .all(|field| value.get(*field).and_then(Value::as_u64).is_some())
                    && (measurement.key != "storage" || valid_tiers(value)) => {}
            _ => return Err(malformed(&format!("{} value", measurement.key))),
        }
    }
    Ok(body)
}

/// Every tier carries its logical and stored bytes.
fn valid_tiers(value: &Map<String, Value>) -> bool {
    value
        .get("tiers")
        .and_then(Value::as_object)
        .is_some_and(|tiers| {
            tiers.values().all(|tier| {
                ["logical_bytes", "stored_bytes"]
                    .iter()
                    .all(|key| tier.get(*key).and_then(Value::as_u64).is_some())
            })
        })
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing)]
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

    fn body() -> Value {
        json!({
            "schema_version": USAGE_SCHEMA,
            "observed_at": "2026-10-26T00:00:00Z",
            "memory": {"scope": "process", "period": "current", "value": {
                "resident_bytes": 10, "budget_bytes": 100, "hot_resident_bytes": 10,
                "warm_resident_bytes": 0, "cold_resident_bytes": 0, "external_bytes": 5}},
            "ingest": {"scope": "process", "period": "since_process_start",
                       "value": {"accepted_points": 7}},
            "storage": {"scope": "tenant", "period": "current", "tenant_id": 0, "value": {
                "tiers": {"hot": {"logical_bytes": 3, "stored_bytes": 3}},
                "hot_metric_names": 1, "hot_metric_rows": 2}},
        })
    }

    #[test]
    fn a_documented_answer_is_read() {
        assert!(read_usage(&response(200, body(), &[])).is_ok());
        let mut unavailable = body();
        unavailable["storage"]["value"] = Value::Null;
        unavailable["storage"]["unavailable_reason"] = json!("the tenant has no store yet");
        assert!(read_usage(&response(200, unavailable, &[])).is_ok());
    }

    #[test]
    fn anything_else_is_malformed_or_partial() {
        let mutations: [fn(&mut Value); 8] = [
            |body| body["schema_version"] = json!("packdb.server.usage.v2"),
            |body| body["ingest"]["period"] = json!("current"),
            |body| body["memory"]["scope"] = json!("tenant"),
            |body| {
                body["storage"]
                    .as_object_mut()
                    .expect("object")
                    .remove("tenant_id");
            },
            // A null value needs the server's reason.
            |body| body["ingest"]["value"] = Value::Null,
            // An omitted value is not a null one.
            |body| {
                body["ingest"]
                    .as_object_mut()
                    .expect("object")
                    .remove("value");
            },
            |body| body["memory"]["value"]["budget_bytes"] = json!(-1),
            |body| body["storage"]["value"]["tiers"]["hot"] = json!({"logical_bytes": 3}),
        ];
        for (index, mutate) in mutations.iter().enumerate() {
            let mut changed = body();
            mutate(&mut changed);
            assert!(
                read_usage(&response(200, changed, &[])).is_err(),
                "mutation {index}"
            );
        }
        assert!(read_usage(&response(503, body(), &[])).is_err());
        assert!(read_usage(&response(200, body(), &["cold_tier_unavailable"])).is_err());
    }

    #[test]
    fn stored_bytes_sum_every_tier() {
        let value = json!({"tiers": {
            "hot": {"logical_bytes": 1, "stored_bytes": 2},
            "warm": {"logical_bytes": 1, "stored_bytes": 5},
        }});
        assert_eq!(stored_bytes(value.as_object().expect("object")), 7);
    }
}
