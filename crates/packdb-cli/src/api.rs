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

//! The shared runner of `query promql`, `query logql`, `query traceql` and
//! `trace`: one request to the server's HTTP query API, reported under the
//! common output and exit contract.

use std::collections::BTreeMap;

use http::{HeaderMap, HeaderValue};
use packdb_client::{
    ClientError, ClientResult, LogOptions, OutputFormat, QueryApiClient, QueryApiConfig,
    QueryResponse,
};
use serde_json::{Value, json};

use crate::Io;
use crate::connection::{ConnectionArgs, UsageError};
use crate::exit::{Exit, error_kind};
use crate::output::{
    Completeness, Envelope, ErrorReport, QueryFormat, TransportReport, completeness_warning,
};
use crate::render::{Rendered, deliver};

/// The request a command sends.
pub(crate) enum Request {
    PromqlInstant {
        query: String,
        time: Option<String>,
    },
    PromqlRange {
        query: String,
        start: String,
        end: String,
        step: String,
    },
    LogqlInstant {
        query: String,
        time: Option<String>,
        options: LogOptions,
    },
    LogqlRange {
        query: String,
        start: String,
        end: String,
        step: Option<String>,
        options: LogOptions,
    },
    TraceqlSearch {
        query: String,
        start: u64,
        end: u64,
        limit: Option<u32>,
    },
    Trace {
        id: String,
        window: Option<(u64, u64)>,
    },
}

/// How a successful response becomes rows for table and CSV output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rows {
    /// A Prometheus or Loki `data.result`: vector, matrix, streams, scalar or string.
    Series,
    /// A Tempo search response's `traces`.
    Traces,
    /// Not tabular: only JSON can represent it.
    None,
}

/// A command ready to run.
pub(crate) struct Plan {
    pub(crate) command: &'static str,
    pub(crate) request: Request,
    pub(crate) rows: Rows,
    /// True only where the API's verified contract establishes that an
    /// absent completeness carrier means a complete result.
    pub(crate) absent_means_complete: bool,
    pub(crate) unknown_reason: &'static str,
}

async fn send(
    client: &QueryApiClient,
    request: Request,
    headers: HeaderMap,
) -> ClientResult<QueryResponse<Value>> {
    match request {
        Request::PromqlInstant { query, time } => {
            client
                .promql_instant(&query, time.as_deref(), headers)
                .await
        }
        Request::PromqlRange {
            query,
            start,
            end,
            step,
        } => {
            client
                .promql_range(&query, &start, &end, &step, headers)
                .await
        }
        Request::LogqlInstant {
            query,
            time,
            options,
        } => {
            client
                .logql_instant(&query, time.as_deref(), options, headers)
                .await
        }
        Request::LogqlRange {
            query,
            start,
            end,
            step,
            options,
        } => {
            client
                .logql_range(&query, &start, &end, step.as_deref(), options, headers)
                .await
        }
        Request::TraceqlSearch {
            query,
            start,
            end,
            limit,
        } => {
            client
                .traceql_search(&query, Some(start), Some(end), limit, headers)
                .await
        }
        Request::Trace { id, window } => {
            let (start, end) = window.unzip();
            client.trace(&id, start, end, headers).await
        }
    }
}

/// Run a planned command. A plan that could not be made is a usage error.
pub(crate) async fn run(
    connection: &ConnectionArgs,
    format: QueryFormat,
    plan: Result<Plan, UsageError>,
    io: Io<'_>,
) -> Exit {
    let Io {
        stdout,
        stderr,
        env,
    } = io;
    let usage =
        |message: &str, stdout: &mut dyn std::io::Write, stderr: &mut dyn std::io::Write| {
            deliver(stdout, stderr, &Rendered::error(message), Exit::Usage)
        };
    let plan = match plan {
        Ok(plan) => plan,
        Err(error) => return usage(&error.0, stdout, stderr),
    };
    if plan.rows == Rows::None && format != QueryFormat::Json {
        return usage(
            &format!("{} supports --format json only", plan.command),
            stdout,
            stderr,
        );
    }
    let connection = match connection.resolve(env) {
        Ok(connection) => connection,
        Err(error) => return usage(&error.0, stdout, stderr),
    };
    let mut headers = HeaderMap::new();
    if let Some(tenant) = &connection.tenant {
        match HeaderValue::from_str(tenant) {
            Ok(value) => {
                headers.insert("x-scope-orgid", value);
            }
            Err(_) => return usage("the tenant is not a valid header value", stdout, stderr),
        }
    }
    let endpoint = connection.endpoint.clone();
    let client = match QueryApiClient::new(QueryApiConfig {
        url: connection.endpoint,
        auth: connection.auth.into_provider(),
        timeout: connection.timeout,
    }) {
        Ok(client) => client,
        Err(error) => return usage(&error.to_string(), stdout, stderr),
    };
    let Plan {
        command,
        request,
        rows,
        absent_means_complete,
        unknown_reason,
    } = plan;
    let mut out = Rendered::default();
    let exit = match send(&client, request, headers).await {
        Ok(response) => {
            let answer = Answer {
                command,
                endpoint,
                format,
                rows,
                absent_means_complete,
                unknown_reason,
            };
            answer.render(response, &mut out)
        }
        Err(error) => render_failure(command, endpoint, format, &error, &mut out),
    };
    deliver(stdout, stderr, &out, exit)
}

struct Answer {
    command: &'static str,
    endpoint: String,
    format: QueryFormat,
    rows: Rows,
    absent_means_complete: bool,
    unknown_reason: &'static str,
}

impl Answer {
    fn render(self, response: QueryResponse<Value>, out: &mut Rendered) -> Exit {
        let QueryResponse {
            data,
            partial_causes,
        } = response;
        let completeness = Completeness::of(&partial_causes, self.absent_means_complete);
        let warnings: Vec<String> =
            completeness_warning(completeness, &partial_causes, self.unknown_reason)
                .into_iter()
                .collect();
        let status = if completeness == Completeness::Complete {
            Exit::Success
        } else {
            Exit::Incomplete
        };
        let rendered = match self.format {
            QueryFormat::Json => Ok(Envelope {
                command: self.command,
                endpoint: self.endpoint,
                transport: http_transport(true),
                completeness: Some(completeness),
                partial_causes,
                warnings: warnings.clone(),
                error: None,
                data,
            }
            .to_json()
            .to_string()),
            QueryFormat::Table => {
                tabulate(self.rows, &data).and_then(|rows| OutputFormat::Table.render(&rows))
            }
            QueryFormat::Csv => {
                tabulate(self.rows, &data).and_then(|rows| OutputFormat::Csv.render(&rows))
            }
        };
        for warning in &warnings {
            Rendered::line(&mut out.stderr, &format!("warning: {warning}"));
        }
        match rendered {
            Ok(text) => {
                if !text.is_empty() {
                    Rendered::line(&mut out.stdout, &text);
                }
                status
            }
            Err(error) => {
                Rendered::line(&mut out.stderr, &format!("error: {error}"));
                Exit::of(&error)
            }
        }
    }
}

fn http_transport(sent: bool) -> TransportReport {
    TransportReport {
        requested: "http",
        used: sent.then_some("http"),
        fallback_reason: None,
    }
}

fn render_failure(
    command: &'static str,
    endpoint: String,
    format: QueryFormat,
    error: &ClientError,
    out: &mut Rendered,
) -> Exit {
    if format == QueryFormat::Json {
        let sent = !matches!(error, ClientError::ConfigError(_));
        let envelope = Envelope {
            command,
            endpoint,
            transport: http_transport(sent),
            completeness: None,
            partial_causes: Vec::new(),
            warnings: Vec::new(),
            error: Some(ErrorReport {
                kind: error_kind(error),
                message: error.to_string(),
            }),
            data: Value::Null,
        };
        Rendered::line(&mut out.stdout, &envelope.to_json().to_string());
    }
    Rendered::line(&mut out.stderr, &format!("error: {error}"));
    Exit::of(error)
}

fn shape_error(what: &str) -> ClientError {
    ClientError::ProtocolError(format!("the server's response has no {what}"))
}

/// Rows for table and CSV output. Every cell is text exactly as the server
/// sent it, so timestamps and sample values are never rounded.
pub(crate) fn tabulate(rows: Rows, body: &Value) -> ClientResult<Vec<Value>> {
    match rows {
        Rows::Series => series_rows(body),
        Rows::Traces => trace_rows(body),
        Rows::None => Err(ClientError::ConfigError(
            "this result has no table form".into(),
        )),
    }
}

fn text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// `{a="1", b="2"}`, keys sorted.
fn labels(value: Option<&Value>) -> String {
    let sorted: BTreeMap<&String, String> = value
        .and_then(Value::as_object)
        .map(|map| map.iter().map(|(k, v)| (k, text(Some(v)))).collect())
        .unwrap_or_default();
    let pairs: Vec<String> = sorted
        .iter()
        .map(|(k, v)| format!("{k}={}", Value::String(v.clone())))
        .collect();
    format!("{{{}}}", pairs.join(", "))
}

fn sample(labels: &str, point: Option<&Value>) -> Value {
    let point = point.and_then(Value::as_array);
    json!({
        "labels": labels,
        "timestamp": text(point.and_then(|p| p.first())),
        "value": text(point.and_then(|p| p.get(1))),
    })
}

fn series_rows(body: &Value) -> ClientResult<Vec<Value>> {
    let data = body.get("data").ok_or_else(|| shape_error("data"))?;
    let kind = data
        .get("resultType")
        .and_then(Value::as_str)
        .ok_or_else(|| shape_error("resultType"))?;
    let result = data.get("result");
    let items = || result.and_then(Value::as_array).into_iter().flatten();
    let mut rows = Vec::new();
    match kind {
        "vector" => {
            for item in items() {
                rows.push(sample(&labels(item.get("metric")), item.get("value")));
            }
        }
        "matrix" => {
            for item in items() {
                let series = labels(item.get("metric"));
                for point in item
                    .get("values")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    rows.push(sample(&series, Some(point)));
                }
            }
        }
        "streams" => {
            for item in items() {
                let stream = labels(item.get("stream"));
                for entry in item
                    .get("values")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let entry = entry.as_array();
                    rows.push(json!({
                        "labels": stream,
                        "timestamp": text(entry.and_then(|e| e.first())),
                        "line": text(entry.and_then(|e| e.get(1))),
                        "metadata": text(entry.and_then(|e| e.get(2))),
                    }));
                }
            }
        }
        "scalar" | "string" => rows.push(sample("{}", result)),
        other => {
            return Err(ClientError::ProtocolError(format!(
                "the server returned result type {other:?}, which has no table form"
            )));
        }
    }
    Ok(rows)
}

fn trace_rows(body: &Value) -> ClientResult<Vec<Value>> {
    let traces = body
        .get("traces")
        .and_then(Value::as_array)
        .ok_or_else(|| shape_error("traces"))?;
    Ok(traces
        .iter()
        .map(|trace| {
            json!({
                "traceID": text(trace.get("traceID")),
                "rootServiceName": text(trace.get("rootServiceName")),
                "rootTraceName": text(trace.get("rootTraceName")),
                "startTimeUnixNano": text(trace.get("startTimeUnixNano")),
                "durationMs": text(trace.get("durationMs")),
            })
        })
        .collect())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn vectors_and_matrices_become_one_row_per_sample() {
        let vector = json!({"status": "success", "data": {"resultType": "vector", "result": [
            {"metric": {"job": "a", "__name__": "up"}, "value": [1700000000.5, "NaN"]}
        ]}});
        let rows = tabulate(Rows::Series, &vector).expect("vector");
        assert_eq!(
            rows,
            [json!({
                "labels": "{__name__=\"up\", job=\"a\"}",
                "timestamp": "1700000000.5",
                "value": "NaN",
            })]
        );
        let matrix = json!({"data": {"resultType": "matrix", "result": [
            {"metric": {}, "values": [[1, "+Inf"], [2, "3"]]}
        ]}});
        let rows = tabulate(Rows::Series, &matrix).expect("matrix");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["value"], "+Inf");
        assert_eq!(rows[1]["labels"], "{}");
    }

    #[test]
    fn streams_keep_nanosecond_timestamps_and_metadata() {
        let streams = json!({"data": {"resultType": "streams", "result": [
            {"stream": {"app": "x"}, "values": [
                ["1700000000123456789", "hello", {"trace_id": "t"}],
                ["1700000000123456790", "world"]
            ]}
        ]}});
        let rows = tabulate(Rows::Series, &streams).expect("streams");
        assert_eq!(rows[0]["timestamp"], "1700000000123456789");
        assert_eq!(rows[0]["line"], "hello");
        assert_eq!(rows[0]["metadata"], "{\"trace_id\":\"t\"}");
        assert_eq!(rows[1]["metadata"], "");
    }

    #[test]
    fn scalars_and_traces_tabulate() {
        let scalar = json!({"data": {"resultType": "scalar", "result": [1, "2"]}});
        assert_eq!(
            tabulate(Rows::Series, &scalar).expect("scalar")[0]["value"],
            "2"
        );
        let search = json!({"traces": [{"traceID": "ab", "rootServiceName": "api",
            "rootTraceName": "GET", "startTimeUnixNano": "17", "durationMs": 5}]});
        let rows = tabulate(Rows::Traces, &search).expect("traces");
        assert_eq!(rows[0]["traceID"], "ab");
        assert_eq!(rows[0]["durationMs"], "5");
    }

    #[test]
    fn shapes_without_a_table_form_are_errors() {
        let cases = [
            (Rows::Series, json!({})),
            (Rows::Series, json!({"data": {}})),
            (
                Rows::Series,
                json!({"data": {"resultType": "histogram", "result": []}}),
            ),
            (Rows::Traces, json!({"traces": "x"})),
            (Rows::None, json!({})),
        ];
        for (rows, body) in cases {
            assert!(tabulate(rows, &body).is_err(), "{rows:?} {body}");
        }
    }
}
