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

//! `packdb smoke-test`: write a small fixture and read this run's data back.
//!
//! The fixture is OTLP/JSON metrics, logs and traces. Every record carries a
//! run marker unique to this run, and every read filters on that marker or on
//! the run's own trace ID, so data from another run can never satisfy a check.
//! Writing is opt-in: without `--allow-write` nothing is sent. Each write is
//! sent once and never retried; its acknowledgement must decode as the OTLP
//! export response, and anything else is never read as zero rejected records.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use http::{HeaderMap, HeaderValue};
use packdb_client::{
    ClientError, LogOptions, QueryApiClient, QueryApiConfig, SqlClient, SqlClientConfig,
    SqlTransport,
};
use serde_json::{Value, json};

use crate::Io;
use crate::connection::{ConnectionArgs, UsageError};
use crate::exit::Exit;
use crate::output::{
    Envelope, ErrorReport, OperatorFormat, ServerNotices, TransportReport, display_endpoint,
};
use crate::probe::{ProbeError, ProbeResponse, Prober};
use crate::render::{Rendered, deliver};
use crate::report::{Finding, Report, Severity, Status};

/// The fixture's contract version, also attached to every resource.
pub const FIXTURE_VERSION: &str = "packdb.smoke-fixture.v1";

const RUN_ID_ATTRIBUTE: &str = "packdb.smoke.run_id";
const RUN_ID_LABEL: &str = "packdb_smoke_run_id";
const SERVICE_FRONTEND: &str = "packdb-smoke-frontend";
const SERVICE_BACKEND: &str = "packdb-smoke-backend";
const METRIC_COUNTER: &str = "packdb_smoke_requests_total";
const METRIC_GAUGE: &str = "packdb_smoke_queue_depth";
const METRIC_INTEGER: &str = "packdb_smoke_batch_size";
const METRIC_HISTOGRAM: &str = "packdb_smoke_request_duration_seconds";

const POLL_INTERVAL: Duration = Duration::from_secs(1);
const MAX_VISIBILITY_SECS: u64 = 300;

/// Arguments of `packdb smoke-test`.
#[derive(Debug, Clone, clap::Args)]
pub struct SmokeTestArgs {
    /// Where to connect and how to authenticate.
    #[command(flatten)]
    pub connection: ConnectionArgs,

    /// Write the fixture to the server. Without it nothing is sent.
    #[arg(long)]
    pub allow_write: bool,

    /// How long to wait for the run's data to become readable, in seconds (at most 300)
    #[arg(long, value_name = "SECS", default_value_t = 60)]
    pub visibility_timeout: u64,

    /// Output format.
    #[arg(long, value_enum, default_value_t = OperatorFormat::Text)]
    pub format: OperatorFormat,
}

/// One run's fixture: OTLP/JSON payloads and the identifiers reads filter on.
#[derive(Debug, Clone)]
pub(crate) struct Fixture {
    pub(crate) run_id: String,
    pub(crate) trace_id: String,
    pub(crate) start_ns: u64,
    pub(crate) end_ns: u64,
    pub(crate) metrics: Value,
    pub(crate) logs: Value,
    pub(crate) traces: Value,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn attribute(key: &str, value: &str) -> Value {
    json!({"key": key, "value": {"stringValue": value}})
}

fn resource(service: &str, run_id: &str) -> Value {
    json!({"attributes": [
        attribute("service.name", service),
        attribute("service.version", "1.0.0"),
        attribute(RUN_ID_ATTRIBUTE, run_id),
        attribute("packdb.smoke.fixture_version", FIXTURE_VERSION),
    ]})
}

fn scope() -> Value {
    json!({"name": "packdb.smoke", "version": FIXTURE_VERSION})
}

impl Fixture {
    /// The fixture for one run: `trace_id` and `span_ids` must be unique to it.
    pub(crate) fn build(
        run_id: &str,
        trace_id: [u8; 16],
        span_ids: [[u8; 8]; 3],
        now_ns: u64,
    ) -> Fixture {
        let start = now_ns;
        let end = now_ns.saturating_add(1_000_000_000);
        let (start_text, end_text) = (start.to_string(), end.to_string());
        let run = attribute(RUN_ID_ATTRIBUTE, run_id);
        let point = |value: (&str, Value)| {
            let mut point = json!({
                "attributes": [run.clone()],
                "startTimeUnixNano": start_text,
                "timeUnixNano": end_text,
            });
            if let Some(point) = point.as_object_mut() {
                point.insert(value.0.to_string(), value.1);
            }
            point
        };
        let metrics = json!({"resourceMetrics": [{
            "resource": resource(SERVICE_FRONTEND, run_id),
            "scopeMetrics": [{"scope": scope(), "metrics": [
                {"name": METRIC_COUNTER, "unit": "1", "sum": {
                    "dataPoints": [point(("asDouble", json!(7.0)))],
                    "aggregationTemporality": 2, "isMonotonic": true}},
                {"name": METRIC_GAUGE, "unit": "1", "gauge": {
                    "dataPoints": [point(("asDouble", json!(3.0)))]}},
                {"name": METRIC_INTEGER, "unit": "1", "gauge": {
                    "dataPoints": [point(("asInt", json!("128")))]}},
                {"name": METRIC_HISTOGRAM, "unit": "s", "histogram": {
                    "dataPoints": [{
                        "attributes": [run.clone()],
                        "startTimeUnixNano": start_text,
                        "timeUnixNano": end_text,
                        "count": "4", "sum": 1.0,
                        "bucketCounts": ["1", "1", "1", "1"],
                        "explicitBounds": [0.1, 0.25, 0.5],
                        "min": 0.05, "max": 0.6}],
                    "aggregationTemporality": 2}},
            ]}],
        }]});
        let log = |offset: u64, severity: (u64, &str), kind: &str| {
            let time = start.saturating_add(offset).to_string();
            json!({
                "timeUnixNano": time, "observedTimeUnixNano": time,
                "severityNumber": severity.0, "severityText": severity.1,
                "body": {"stringValue": format!("smoke fixture {kind} record")},
                "attributes": [run.clone(), attribute("log.kind", kind)],
            })
        };
        let logs = json!({"resourceLogs": [{
            "resource": resource(SERVICE_FRONTEND, run_id),
            "scopeLogs": [{"scope": scope(), "logRecords": [
                log(0, (9, "INFO"), "structured"),
                log(1, (13, "WARN"), "unstructured"),
            ]}],
        }]});
        let trace_hex = hex(&trace_id);
        let [root, lookup, write] = span_ids.map(|id| hex(&id));
        let span = |id: &str, parent: Option<&str>, name: &str, kind: u8, status: Value| {
            let mut span = json!({
                "traceId": trace_hex, "spanId": id, "name": name, "kind": kind,
                "startTimeUnixNano": start_text, "endTimeUnixNano": end_text,
                "attributes": [run.clone()], "status": status,
            });
            if let (Some(parent), Some(span)) = (parent, span.as_object_mut()) {
                span.insert("parentSpanId".into(), json!(parent));
            }
            span
        };
        let traces = json!({"resourceSpans": [
            {"resource": resource(SERVICE_FRONTEND, run_id),
             "scopeSpans": [{"scope": scope(), "spans": [
                 span(&root, None, "GET /smoke", 2, json!({"code": 1}))]}]},
            {"resource": resource(SERVICE_BACKEND, run_id),
             "scopeSpans": [{"scope": scope(), "spans": [
                 span(&lookup, Some(&root), "backend.lookup", 1, json!({"code": 1})),
                 span(&write, Some(&root), "backend.write", 1,
                      json!({"code": 2, "message": "expected smoke fixture error"}))]}]},
        ]});
        Fixture {
            run_id: run_id.to_string(),
            trace_id: trace_hex,
            start_ns: start,
            end_ns: end,
            metrics,
            logs,
            traces,
        }
    }

    /// A fixture with a fresh run marker and fresh identifiers.
    fn fresh() -> Fixture {
        let run = uuid::Uuid::new_v4();
        let trace = uuid::Uuid::new_v4();
        let spans = [0, 1, 2].map(|_| {
            let mut id = [0_u8; 8];
            id.copy_from_slice(&uuid::Uuid::new_v4().as_bytes()[..8]);
            id
        });
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX))
            .unwrap_or(0);
        Fixture::build(
            &format!("smoke-{}", run.simple()),
            *trace.as_bytes(),
            spans,
            now,
        )
    }
}

/// An OTLP export response. The metrics, logs and traces responses share this
/// layout: `partial_success` (1) holding the rejected count (1) and a message (2).
#[derive(Clone, PartialEq, prost::Message)]
struct ExportResponse {
    #[prost(message, optional, tag = "1")]
    partial_success: Option<PartialSuccess>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct PartialSuccess {
    #[prost(int64, tag = "1")]
    rejected: i64,
    #[prost(string, tag = "2")]
    error_message: String,
}

/// What an acknowledgement says was rejected: an empty body rejected nothing;
/// otherwise it must be a protobuf export response with no field it does not
/// define. Anything else is an error, never zero.
fn rejected_by(response: &ProbeResponse) -> Result<(i64, String), String> {
    if response.body.is_empty() {
        return Ok((0, String::new()));
    }
    let protobuf = response.content_type.as_deref().is_some_and(|value| {
        value
            .split(';')
            .next()
            .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/x-protobuf"))
    });
    if !protobuf {
        return Err(format!(
            "the acknowledgement is not application/x-protobuf ({})",
            response
                .content_type
                .as_deref()
                .unwrap_or("no content type")
        ));
    }
    let decoded = <ExportResponse as prost::Message>::decode(response.body.as_slice())
        .map_err(|error| format!("the acknowledgement does not decode: {error}"))?;
    if prost::Message::encoded_len(&decoded) != response.body.len() {
        return Err("the acknowledgement holds fields an OTLP export response does not".into());
    }
    let partial = decoded.partial_success.unwrap_or_default();
    if partial.rejected < 0 {
        return Err(format!(
            "the acknowledgement reports {} rejected records",
            partial.rejected
        ));
    }
    Ok((partial.rejected, partial.error_message))
}

pub(crate) async fn run(args: SmokeTestArgs, io: Io<'_>) -> Exit {
    let Io {
        stdout,
        stderr,
        env,
    } = io;
    let usage =
        |message: &str, stdout: &mut dyn std::io::Write, stderr: &mut dyn std::io::Write| {
            deliver(stdout, stderr, &Rendered::error(message), Exit::Usage)
        };
    if !args.allow_write {
        return usage(
            "smoke-test writes a fixture to the server; pass --allow-write to send it",
            stdout,
            stderr,
        );
    }
    if args.visibility_timeout == 0 || args.visibility_timeout > MAX_VISIBILITY_SECS {
        return usage(
            &format!("--visibility-timeout must be 1 to {MAX_VISIBILITY_SECS} seconds"),
            stdout,
            stderr,
        );
    }
    let connection = match args.connection.resolve(env) {
        Ok(connection) => connection,
        Err(error) => return usage(&error.0, stdout, stderr),
    };
    let readers = match Readers::new(&connection) {
        Ok(readers) => readers,
        Err(error) => return usage(&error.0, stdout, stderr),
    };
    let endpoint = connection.endpoint.clone();
    let tenant_supplied = connection.tenant.is_some();
    let prober = match Prober::new(connection) {
        Ok(prober) => prober,
        Err(error) => return usage(&error.0, stdout, stderr),
    };
    let fixture = Fixture::fresh();
    let mut smoke = Smoke::new(
        &endpoint,
        tenant_supplied,
        &fixture,
        args.visibility_timeout,
    );
    smoke
        .check(
            &prober,
            &readers,
            &fixture,
            Duration::from_secs(args.visibility_timeout),
        )
        .await;
    smoke.answered = prober.responded() || smoke.read_answered;
    smoke.report.finished_at = SystemTime::now();
    let out = smoke.render(&endpoint, &fixture, args.format);
    deliver(stdout, stderr, &out, smoke.exit())
}

/// The query clients reads use, with the connection's credential and tenant.
struct Readers {
    query: QueryApiClient,
    sql: SqlClient,
    headers: HeaderMap,
}

impl Readers {
    fn new(connection: &crate::connection::Connection) -> Result<Readers, UsageError> {
        let mut headers = HeaderMap::new();
        if let Some(tenant) = &connection.tenant {
            let value = HeaderValue::from_str(tenant)
                .map_err(|_| UsageError("the tenant is not a valid header value".into()))?;
            headers.insert("x-scope-orgid", value);
        }
        let query = QueryApiClient::new(QueryApiConfig {
            url: connection.endpoint.clone(),
            auth: connection.auth.clone().into_provider(),
            timeout: connection.timeout,
        })
        .map_err(|error| UsageError(error.to_string()))?;
        let sql = SqlClient::new(SqlClientConfig {
            url: connection.endpoint.clone(),
            flight_url: None,
            auth: connection.auth.clone().into_provider(),
            timeout: connection.timeout,
            transport: SqlTransport::Http,
        });
        Ok(Readers {
            query,
            sql,
            headers,
        })
    }
}

/// The result of one read attempt.
enum Read {
    /// The read is graded.
    Done(Finding),
    /// The run's data is not visible yet, or does not match yet.
    NotYet(String),
    /// No usable answer: a failure that stays missing evidence at the deadline.
    Unusable(ClientFailure),
}

struct ClientFailure {
    reason: String,
    exit: Exit,
}

impl ClientFailure {
    fn of(error: &ClientError) -> ClientFailure {
        let exit = match error {
            ClientError::Unauthenticated(_) => Exit::Unauthenticated,
            ClientError::PermissionDenied(_) => Exit::PermissionDenied,
            ClientError::ConnectionError(_) => Exit::Unreachable,
            _ => Exit::Incomplete,
        };
        ClientFailure {
            reason: error.to_string(),
            exit,
        }
    }
}

/// One smoke-test run.
struct Smoke {
    report: Report,
    failures: Vec<Exit>,
    first_error: Option<(&'static str, String)>,
    answered: bool,
    read_answered: bool,
}

/// The reads, each named by its finding and tied to the signal it needs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Check {
    PromqlCounter,
    PromqlGauge,
    PromqlInteger,
    PromqlHistogramCount,
    LogqlRecords,
    TraceqlSearch,
    TraceSpans,
    JaegerSpans,
    SqlErrorSpan,
    SqlServiceSpans,
}

impl Check {
    const ALL: [Check; 10] = [
        Check::PromqlCounter,
        Check::PromqlGauge,
        Check::PromqlInteger,
        Check::PromqlHistogramCount,
        Check::LogqlRecords,
        Check::TraceqlSearch,
        Check::TraceSpans,
        Check::JaegerSpans,
        Check::SqlErrorSpan,
        Check::SqlServiceSpans,
    ];

    fn id(self) -> &'static str {
        match self {
            Check::PromqlCounter => "read.promql.counter",
            Check::PromqlGauge => "read.promql.gauge",
            Check::PromqlInteger => "read.promql.integer",
            Check::PromqlHistogramCount => "read.promql.histogram_count",
            Check::LogqlRecords => "read.logql.records",
            Check::TraceqlSearch => "read.traceql.search",
            Check::TraceSpans => "read.trace.spans",
            Check::JaegerSpans => "read.jaeger.spans",
            Check::SqlErrorSpan => "read.sql.error_span",
            Check::SqlServiceSpans => "read.sql.service_spans",
        }
    }

    fn signal(self) -> &'static str {
        match self {
            Check::PromqlCounter
            | Check::PromqlGauge
            | Check::PromqlInteger
            | Check::PromqlHistogramCount => "metrics",
            Check::LogqlRecords => "logs",
            _ => "traces",
        }
    }
}

impl Smoke {
    fn new(endpoint: &str, tenant_supplied: bool, fixture: &Fixture, visibility: u64) -> Smoke {
        let mut report = Report::new("smoke_test");
        report
            .target
            .insert("endpoint", display_endpoint(endpoint).into());
        report
            .target
            .insert("tenant_supplied", tenant_supplied.into());
        report.scope.insert("run_id", fixture.run_id.clone().into());
        report
            .scope
            .insert("trace_id", fixture.trace_id.clone().into());
        report
            .scope
            .insert("fixture_version", FIXTURE_VERSION.into());
        report
            .scope
            .insert("visibility_timeout_secs", visibility.into());
        Smoke {
            report,
            failures: Vec::new(),
            first_error: None,
            answered: false,
            read_answered: false,
        }
    }

    fn record_failure(&mut self, exit: Exit, kind: &'static str, reason: &str) {
        if exit != Exit::Incomplete {
            self.failures.push(exit);
            if self.first_error.is_none() {
                self.first_error = Some((kind, reason.to_string()));
            }
        }
    }

    async fn check(
        &mut self,
        prober: &Prober,
        readers: &Readers,
        fixture: &Fixture,
        visibility: Duration,
    ) {
        let mut written = Vec::new();
        let mut stop = None;
        for (signal, path, body) in [
            ("metrics", "/v1/metrics", &fixture.metrics),
            ("logs", "/v1/logs", &fixture.logs),
            ("traces", "/v1/traces", &fixture.traces),
        ] {
            let id = match signal {
                "metrics" => "write.metrics",
                "logs" => "write.logs",
                _ => "write.traces",
            };
            if let Some(reason) = &stop {
                self.report.uncollected(
                    id,
                    Severity::Required,
                    &format!("the {signal} write"),
                    &format!("not sent: {reason}"),
                );
                continue;
            }
            match prober
                .post_json_once(path, body.to_string().into_bytes())
                .await
            {
                Ok(response) => {
                    if self.grade_write(id, signal, path, &response) {
                        written.push(signal);
                    }
                }
                Err(error) => {
                    let reason = error.message().to_string();
                    let exit = error.exit();
                    if matches!(
                        error,
                        ProbeError::Unauthenticated(_)
                            | ProbeError::PermissionDenied(_)
                            | ProbeError::Unreachable(_)
                    ) {
                        stop = Some(reason.clone());
                    }
                    self.record_failure(exit, probe_error_kind(&error), &reason);
                    self.report.uncollected(
                        id,
                        Severity::Required,
                        &format!("the {signal} write"),
                        &reason,
                    );
                }
            }
        }
        let mut pending: Vec<Check> = Vec::new();
        for check in Check::ALL {
            if written.contains(&check.signal()) {
                pending.push(check);
            } else {
                self.report.uncollected(
                    check.id(),
                    Severity::Required,
                    check.id(),
                    &format!("not read: the {} write was not accepted", check.signal()),
                );
            }
        }
        self.read_until(prober, readers, fixture, pending, visibility)
            .await;
    }

    /// Grade one write's acknowledgement; true when everything was accepted.
    fn grade_write(
        &mut self,
        id: &'static str,
        signal: &str,
        path: &str,
        response: &ProbeResponse,
    ) -> bool {
        if !response.status.is_success() {
            self.report.push(
                Finding::new(
                    id,
                    Severity::Required,
                    Status::Fail,
                    format!(
                        "the server refused the {signal} write: {path} answered {}",
                        response.status.as_u16()
                    ),
                )
                .with("status", response.status.as_u16()),
            );
            return false;
        }
        match rejected_by(response) {
            Ok((0, _)) => {
                self.report.push(
                    Finding::new(
                        id,
                        Severity::Required,
                        Status::Pass,
                        format!("the server accepted every {signal} record"),
                    )
                    .with("rejected", 0),
                );
                true
            }
            Ok((rejected, message)) => {
                self.report.push(
                    Finding::new(
                        id,
                        Severity::Required,
                        Status::Fail,
                        format!("the server rejected {rejected} {signal} record(s): {message}"),
                    )
                    .with("rejected", rejected),
                );
                false
            }
            Err(reason) => {
                self.report.uncollected(
                    id,
                    Severity::Required,
                    &format!("the {signal} acknowledgement"),
                    &reason,
                );
                false
            }
        }
    }

    /// Poll every pending read until it is graded or the visibility deadline
    /// passes. Each read gets only the time left before the deadline. A read
    /// that cannot improve by waiting — a refused credential, an unreachable
    /// server — is recorded at once. At the deadline a read whose data never
    /// matched fails; one that never got a usable answer, or never finished,
    /// is missing evidence.
    async fn read_until(
        &mut self,
        prober: &Prober,
        readers: &Readers,
        fixture: &Fixture,
        mut pending: Vec<Check>,
        visibility: Duration,
    ) {
        let deadline = Instant::now() + visibility;
        let mut last: Vec<(Check, Read)> = Vec::new();
        loop {
            let mut next = Vec::new();
            for check in std::mem::take(&mut pending) {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    next.push(check);
                    continue;
                }
                let Ok(read) =
                    tokio::time::timeout(remaining, read(check, prober, readers, fixture)).await
                else {
                    next.push(check);
                    continue;
                };
                last.retain(|(seen, _)| *seen != check);
                match read {
                    Read::Done(finding) => {
                        self.read_answered = true;
                        self.report.push(finding);
                    }
                    Read::NotYet(reason) => {
                        self.read_answered = true;
                        next.push(check);
                        last.push((check, Read::NotYet(reason)));
                    }
                    Read::Unusable(failure) if failure.exit != Exit::Incomplete => {
                        self.record_failure(
                            failure.exit,
                            client_exit_kind(failure.exit),
                            &failure.reason,
                        );
                        self.report.uncollected(
                            check.id(),
                            Severity::Required,
                            check.id(),
                            &failure.reason,
                        );
                    }
                    Read::Unusable(failure) => {
                        next.push(check);
                        last.push((check, Read::Unusable(failure)));
                    }
                }
            }
            pending = next;
            if pending.is_empty() || Instant::now() + POLL_INTERVAL > deadline {
                break;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        for check in pending {
            let latest = last
                .iter()
                .position(|(seen, _)| *seen == check)
                .map(|index| last.swap_remove(index).1);
            match latest {
                Some(Read::NotYet(reason)) => self.report.push(Finding::new(
                    check.id(),
                    Severity::Required,
                    Status::Fail,
                    format!("not visible within {}s: {reason}", visibility.as_secs()),
                )),
                Some(Read::Unusable(failure)) => self.report.uncollected(
                    check.id(),
                    Severity::Required,
                    check.id(),
                    &failure.reason,
                ),
                _ => self.report.uncollected(
                    check.id(),
                    Severity::Required,
                    check.id(),
                    &format!(
                        "no answer before the {}s visibility timeout",
                        visibility.as_secs()
                    ),
                ),
            }
        }
    }

    fn exit(&self) -> Exit {
        self.failures
            .iter()
            .filter(|exit| !(self.answered && **exit == Exit::Unreachable))
            .fold(self.report.exit(), |exit, failure| exit.worst(*failure))
    }

    fn render(&self, endpoint: &str, fixture: &Fixture, format: OperatorFormat) -> Rendered {
        let mut out = Rendered::default();
        let exit = self.exit();
        let warning = (!self.report.is_complete()).then(|| {
            format!(
                "completeness unknown ({} piece(s) of required evidence could not be collected)",
                self.report.missing_evidence.len().max(1)
            )
        });
        let error = self.first_error.as_ref().filter(|_| {
            exit != Exit::Incomplete && exit != Exit::Rejected && exit != Exit::Success
        });
        match format {
            OperatorFormat::Json => {
                let envelope = Envelope {
                    command: "smoke-test",
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
                    error: error.map(|(kind, message)| ErrorReport {
                        kind,
                        message: message.clone(),
                    }),
                    data: self.report.to_json(),
                };
                Rendered::line(&mut out.stdout, &envelope.to_json().to_string());
            }
            OperatorFormat::Text => {
                Rendered::line(
                    &mut out.stdout,
                    &format!(
                        "packdb smoke-test: {} (run {})",
                        display_endpoint(endpoint),
                        fixture.run_id
                    ),
                );
                out.stdout.push_str(&self.report.to_text());
            }
        }
        if self.answered {
            Rendered::line(
                &mut out.stderr,
                &format!(
                    "note: wrote smoke fixture run {} (trace {}) to the server",
                    fixture.run_id, fixture.trace_id
                ),
            );
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
        if let Some((_, message)) = error {
            Rendered::line(&mut out.stderr, &format!("error: {message}"));
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

fn client_exit_kind(exit: Exit) -> &'static str {
    match exit {
        Exit::Unauthenticated => "unauthenticated",
        Exit::PermissionDenied => "permission_denied",
        Exit::Unreachable => "unreachable",
        _ => "server_or_protocol",
    }
}

/// Grade a counted result: `Done` when it matches, `NotYet` otherwise.
fn count(check: Check, found: usize, expected: usize, what: &str) -> Read {
    if found == expected {
        Read::Done(
            Finding::new(
                check.id(),
                Severity::Required,
                Status::Pass,
                format!("{found} {what} of this run read back"),
            )
            .with("count", found),
        )
    } else {
        Read::NotYet(format!("{found} {what} of this run, expected {expected}"))
    }
}

/// Every element of every `spans` array in a trace answer, Tempo or Jaeger.
fn span_count(value: &Value) -> usize {
    match value {
        Value::Object(map) => map
            .iter()
            .map(|(key, value)| match (key.as_str(), value) {
                ("spans", Value::Array(spans)) => spans.len(),
                _ => span_count(value),
            })
            .sum(),
        Value::Array(items) => items.iter().map(span_count).sum(),
        _ => 0,
    }
}

async fn read(check: Check, prober: &Prober, readers: &Readers, fixture: &Fixture) -> Read {
    let headers = || readers.headers.clone();
    let run = &fixture.run_id;
    let selector = format!("{RUN_ID_LABEL}=\"{run}\"");
    let promql = |metric: &str, expected: f64| {
        let query = format!("sum({metric}{{{selector}}})");
        // Just after the run's last sample, rounded up: an instant query only
        // sees samples at or before its evaluation time.
        let time = format!("{}", fixture.end_ns.div_ceil(1_000_000_000) + 1);
        async move {
            match readers
                .query
                .promql_instant(&query, Some(&time), headers())
                .await
            {
                Ok(answer) if !answer.partial_causes.is_empty() => Read::NotYet(format!(
                    "partial result ({})",
                    answer.partial_causes.join(", ")
                )),
                Ok(answer) => {
                    let value = answer
                        .data
                        .pointer("/data/result/0/value/1")
                        .and_then(Value::as_str)
                        .and_then(|text| text.parse::<f64>().ok());
                    match value {
                        Some(value) if (value - expected).abs() < 1e-6 => Read::Done(
                            Finding::new(
                                check.id(),
                                Severity::Required,
                                Status::Pass,
                                format!("{query} = {value}"),
                            )
                            .with("value", value),
                        ),
                        Some(value) => {
                            Read::NotYet(format!("{query} = {value}, expected {expected}"))
                        }
                        None => Read::NotYet(format!("{query} returned no sample")),
                    }
                }
                Err(error) => Read::Unusable(ClientFailure::of(&error)),
            }
        }
    };
    match check {
        Check::PromqlCounter => promql(METRIC_COUNTER, 7.0).await,
        Check::PromqlGauge => promql(METRIC_GAUGE, 3.0).await,
        Check::PromqlInteger => promql(METRIC_INTEGER, 128.0).await,
        Check::PromqlHistogramCount => promql(&format!("{METRIC_HISTOGRAM}_count"), 4.0).await,
        Check::LogqlRecords => {
            let query =
                format!("{{service_name=\"{SERVICE_FRONTEND}\"}} | {RUN_ID_LABEL} = `{run}`");
            let start = fixture.start_ns.saturating_sub(1_000_000_000).to_string();
            let end = fixture.end_ns.saturating_add(1_000_000_000).to_string();
            let options = LogOptions {
                limit: Some(100),
                direction: None,
            };
            match readers
                .query
                .logql_range(&query, &start, &end, None, options, headers())
                .await
            {
                Ok(answer) if !answer.partial_causes.is_empty() => Read::NotYet(format!(
                    "partial result ({})",
                    answer.partial_causes.join(", ")
                )),
                Ok(answer) => {
                    let lines = answer
                        .data
                        .pointer("/data/result")
                        .and_then(Value::as_array)
                        .map(|streams| {
                            streams
                                .iter()
                                .map(|stream| {
                                    stream
                                        .get("values")
                                        .and_then(Value::as_array)
                                        .map_or(0, Vec::len)
                                })
                                .sum()
                        })
                        .unwrap_or(0);
                    count(check, lines, 2, "log record(s)")
                }
                Err(error) => Read::Unusable(ClientFailure::of(&error)),
            }
        }
        Check::TraceqlSearch => {
            let query = format!("{{ span.{RUN_ID_ATTRIBUTE} = \"{run}\" }}");
            let start = fixture.start_ns / 1_000_000_000;
            let end = fixture.end_ns / 1_000_000_000 + 2;
            match readers
                .query
                .traceql_search(&query, Some(start), Some(end), Some(20), headers())
                .await
            {
                Ok(answer) if !answer.partial_causes.is_empty() => Read::NotYet(format!(
                    "partial result ({})",
                    answer.partial_causes.join(", ")
                )),
                Ok(answer) => {
                    let traces = answer
                        .data
                        .get("traces")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    let ours = traces
                        .iter()
                        .filter(|trace| {
                            trace.get("traceID").and_then(Value::as_str)
                                == Some(fixture.trace_id.as_str())
                        })
                        .count();
                    count(check, ours, 1, "trace(s)")
                }
                Err(error) => Read::Unusable(ClientFailure::of(&error)),
            }
        }
        Check::TraceSpans => {
            match readers
                .query
                .trace(&fixture.trace_id, None, None, headers())
                .await
            {
                Ok(answer) if !answer.partial_causes.is_empty() => Read::NotYet(format!(
                    "partial result ({})",
                    answer.partial_causes.join(", ")
                )),
                Ok(answer) => count(check, span_count(&answer.data), 3, "span(s)"),
                Err(ClientError::NotFound(message)) => Read::NotYet(message),
                Err(error) => Read::Unusable(ClientFailure::of(&error)),
            }
        }
        Check::JaegerSpans => {
            let path = format!("/jaeger/api/traces/{}", fixture.trace_id);
            match prober.get(&path).await {
                Ok(response) if !response.partial_causes.is_empty() => Read::NotYet(format!(
                    "partial result ({})",
                    response.partial_causes.join(", ")
                )),
                Ok(response) if response.status == http::StatusCode::NOT_FOUND => {
                    Read::NotYet(format!("{path} answered 404"))
                }
                Ok(response) if response.status.is_success() => match response.json() {
                    Some(body) => count(
                        check,
                        span_count(body.get("data").unwrap_or(&Value::Null)),
                        3,
                        "span(s)",
                    ),
                    None => Read::Unusable(ClientFailure {
                        reason: format!("{path} answered with unreadable JSON"),
                        exit: Exit::Incomplete,
                    }),
                },
                Ok(response) => Read::Unusable(ClientFailure {
                    reason: format!("{path} answered {}", response.status.as_u16()),
                    exit: Exit::Incomplete,
                }),
                Err(error) => Read::Unusable(ClientFailure {
                    reason: error.message().to_string(),
                    exit: error.exit(),
                }),
            }
        }
        Check::SqlErrorSpan => {
            let sql = format!(
                "SELECT name FROM traces.{} WHERE status_code = 2 AND trace_id = '{}'",
                SERVICE_BACKEND.replace('-', "_"),
                fixture.trace_id
            );
            sql_rows(check, readers, &sql, |rows| {
                let names: Vec<&str> = rows.iter().filter_map(|row| row["name"].as_str()).collect();
                (
                    names == ["backend.write"],
                    format!("error span(s) {names:?}"),
                )
            })
            .await
        }
        Check::SqlServiceSpans => {
            let mut total = 0;
            for (service, expected) in [(SERVICE_FRONTEND, 1), (SERVICE_BACKEND, 2)] {
                let sql = format!(
                    "SELECT span_id FROM traces.{} WHERE trace_id = '{}'",
                    service.replace('-', "_"),
                    fixture.trace_id
                );
                match readers
                    .sql
                    .execute_query_rows_with_transport(&sql, headers())
                    .await
                {
                    Ok(answer) if !answer.response.partial_causes.is_empty() => {
                        return Read::NotYet(format!(
                            "partial result ({})",
                            answer.response.partial_causes.join(", ")
                        ));
                    }
                    Ok(answer) if answer.response.data.len() == expected => total += expected,
                    Ok(answer) => {
                        return Read::NotYet(format!(
                            "{} span(s) in traces.{}, expected {expected}",
                            answer.response.data.len(),
                            service.replace('-', "_")
                        ));
                    }
                    Err(failure) => return sql_failure(&failure.error),
                }
            }
            count(check, total, 3, "span(s) across both service views")
        }
    }
}

async fn sql_rows(
    check: Check,
    readers: &Readers,
    sql: &str,
    grade: impl Fn(&[Value]) -> (bool, String),
) -> Read {
    match readers
        .sql
        .execute_query_rows_with_transport(sql, readers.headers.clone())
        .await
    {
        Ok(answer) if !answer.response.partial_causes.is_empty() => Read::NotYet(format!(
            "partial result ({})",
            answer.response.partial_causes.join(", ")
        )),
        Ok(answer) => match grade(&answer.response.data) {
            (true, summary) => Read::Done(Finding::new(
                check.id(),
                Severity::Required,
                Status::Pass,
                format!("{summary} read back"),
            )),
            (false, summary) => Read::NotYet(summary),
        },
        Err(failure) => sql_failure(&failure.error),
    }
}

/// A SQL error before the run's data is visible names a view that does not
/// exist yet; that is not yet visible, not a failure.
fn sql_failure(error: &ClientError) -> Read {
    match error {
        ClientError::SqlError(message) => Read::NotYet(message.clone()),
        error => Read::Unusable(ClientFailure::of(error)),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn fixture() -> Fixture {
        Fixture::build(
            "smoke-test-run",
            [7; 16],
            [[1; 8], [2; 8], [3; 8]],
            1_700_000_000_000_000_000,
        )
    }

    fn ack(content_type: Option<&str>, body: Vec<u8>) -> ProbeResponse {
        ProbeResponse {
            status: http::StatusCode::OK,
            content_type: content_type.map(str::to_string),
            body,
            partial_causes: Vec::new(),
        }
    }

    #[test]
    fn every_record_carries_the_run_marker() {
        let fixture = fixture();
        assert_eq!(fixture.trace_id, "07".repeat(16));
        for payload in [&fixture.metrics, &fixture.logs, &fixture.traces] {
            let text = payload.to_string();
            assert!(text.contains("smoke-test-run"), "{text}");
        }
        let spans = &fixture.traces["resourceSpans"];
        assert_eq!(span_count(spans), 3);
        assert_eq!(spans[1]["scopeSpans"][0]["spans"][1]["status"]["code"], 2);
        assert_eq!(
            fixture.metrics["resourceMetrics"][0]["scopeMetrics"][0]["metrics"][2]["gauge"]["dataPoints"]
                [0]["asInt"],
            "128"
        );
        assert_eq!(fixture.end_ns - fixture.start_ns, 1_000_000_000);
    }

    #[test]
    fn acknowledgements_decode_or_are_refused() {
        let encode = |rejected: i64, message: &str| {
            prost::Message::encode_to_vec(&ExportResponse {
                partial_success: Some(PartialSuccess {
                    rejected,
                    error_message: message.into(),
                }),
            })
        };
        let protobuf = Some("application/x-protobuf");
        assert_eq!(
            rejected_by(&ack(protobuf, Vec::new())),
            Ok((0, String::new()))
        );
        assert_eq!(rejected_by(&ack(None, Vec::new())), Ok((0, String::new())));
        assert_eq!(
            rejected_by(&ack(protobuf, encode(2, "too old"))),
            Ok((2, "too old".to_string()))
        );
        assert_eq!(
            rejected_by(&ack(
                Some("Application/X-Protobuf; charset=binary"),
                encode(0, "")
            )),
            Ok((0, String::new()))
        );
        // Malformed, unrecognized or mislabelled bytes are never zero.
        for (content_type, body) in [
            (protobuf, vec![0xff, 0xff, 0xff]),
            (protobuf, vec![0x10, 0x01]),
            (Some("application/json"), b"{}".to_vec()),
            (None, encode(0, "")),
            (protobuf, encode(-1, "")),
        ] {
            assert!(
                rejected_by(&ack(content_type, body.clone())).is_err(),
                "{content_type:?} {body:?}"
            );
        }
    }

    #[test]
    fn span_counts_cover_tempo_and_jaeger_shapes() {
        let tempo = json!({"trace": {"batches": [
            {"scopeSpans": [{"spans": [{}, {}]}]},
            {"scopeSpans": [{"spans": [{}]}]}]}});
        assert_eq!(span_count(&tempo), 3);
        let jaeger = json!([{"traceID": "x", "spans": [{}, {}, {}], "processes": {}}]);
        assert_eq!(span_count(&jaeger), 3);
        assert_eq!(span_count(&json!({"trace": {}})), 0);
    }

    #[test]
    fn every_check_is_tied_to_its_signal() {
        let metrics = Check::ALL
            .iter()
            .filter(|check| check.signal() == "metrics")
            .count();
        assert_eq!(metrics, 4);
        assert_eq!(Check::LogqlRecords.signal(), "logs");
        assert_eq!(Check::SqlServiceSpans.signal(), "traces");
        let smoke = Smoke::new("http://h", false, &fixture(), 5);
        assert!(smoke.report.missing_evidence.is_empty());
        assert_eq!(
            smoke.report.scope.get("run_id"),
            Some(&json!("smoke-test-run"))
        );
    }
}
