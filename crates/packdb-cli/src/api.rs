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
    Completeness, Envelope, ErrorReport, QueryFormat, ServerNotices, TransportReport,
    completeness_warning,
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
        // A response that is not the shape its API defines is a protocol
        // failure in every format, never an empty table or a blank cell.
        let checked = check(self.rows, &data).and_then(|rows| Ok((rows, notices(&data)?)));
        let (rows, server) = match checked {
            Ok(checked) => checked,
            Err(error) => {
                return render_failure(self.command, self.endpoint, self.format, &error, out);
            }
        };
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
        for warning in &warnings {
            Rendered::line(&mut out.stderr, &format!("warning: {warning}"));
        }
        for warning in &server.warnings {
            Rendered::line(&mut out.stderr, &format!("warning: server: {warning}"));
        }
        for info in &server.infos {
            Rendered::line(&mut out.stderr, &format!("note: server: {info}"));
        }
        let rendered = match self.format {
            QueryFormat::Json => Ok(Envelope {
                command: self.command,
                endpoint: self.endpoint,
                transport: http_transport(true),
                completeness: Some(completeness),
                partial_causes,
                warnings,
                server,
                error: None,
                data,
            }
            .to_json()
            .to_string()),
            QueryFormat::Table => OutputFormat::Table.render(&rows),
            QueryFormat::Csv => OutputFormat::Csv.render(&rows),
        };
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
            server: ServerNotices::default(),
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

fn malformed(what: &str) -> ClientError {
    ClientError::ProtocolError(format!("malformed response from the server: {what}"))
}

/// Check a response against the shape its API defines and return its table
/// rows. A trace has no table form, so only its shape is checked.
fn check(rows: Rows, body: &Value) -> ClientResult<Vec<Value>> {
    match rows {
        Rows::Series => series_rows(body),
        Rows::Traces => trace_rows(body),
        Rows::None => {
            body.get("trace")
                .and_then(Value::as_object)
                .ok_or_else(|| malformed("no `trace` object"))?;
            Ok(Vec::new())
        }
    }
}

/// The server's `warnings` and `infos` lists. Absent or `null` lists are
/// empty; anything other than a list of strings is malformed.
fn notices(body: &Value) -> ClientResult<ServerNotices> {
    Ok(ServerNotices {
        warnings: strings(body, "warnings")?,
        infos: strings(body, "infos")?,
    })
}

fn strings(body: &Value, key: &str) -> ClientResult<Vec<String>> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| malformed(&format!("`{key}` holds a value that is not text")))
            })
            .collect(),
        Some(_) => Err(malformed(&format!("`{key}` is not a list"))),
    }
}

fn field<'a>(object: &'a Value, key: &str, within: &str) -> ClientResult<&'a Value> {
    object
        .get(key)
        .ok_or_else(|| malformed(&format!("{within} has no `{key}`")))
}

fn list<'a>(value: &'a Value, what: &str) -> ClientResult<&'a Vec<Value>> {
    value
        .as_array()
        .ok_or_else(|| malformed(&format!("`{what}` is not a list")))
}

fn string<'a>(value: &'a Value, what: &str) -> ClientResult<&'a str> {
    value
        .as_str()
        .ok_or_else(|| malformed(&format!("`{what}` is not text")))
}

/// `{a="1", b="2"}`, names sorted. Every label value must be text.
fn labels(item: &Value, key: &str) -> ClientResult<String> {
    let map = field(item, key, "a series")?
        .as_object()
        .ok_or_else(|| malformed(&format!("`{key}` is not an object")))?;
    let mut pairs = Vec::with_capacity(map.len());
    for (name, value) in map {
        pairs.push((name, string(value, &format!("{key}.{name}"))?));
    }
    pairs.sort();
    let pairs: Vec<String> = pairs
        .iter()
        .map(|(name, value)| format!("{name}={}", Value::String((*value).to_string())))
        .collect();
    Ok(format!("{{{}}}", pairs.join(", ")))
}

/// A `[timestamp, "value"]` sample: a numeric timestamp and a text value,
/// both exactly as the server sent them.
fn sample(labels: &str, point: &Value) -> ClientResult<Value> {
    let (timestamp, value) = match list(point, "sample")?.as_slice() {
        [timestamp @ Value::Number(_), Value::String(value)] => (timestamp, value),
        _ => return Err(malformed("a sample is not [timestamp, \"value\"]")),
    };
    Ok(json!({
        "labels": labels,
        "timestamp": timestamp.to_string(),
        "value": value,
    }))
}

/// A `["ns", "line"]` or `["ns", "line", {metadata}]` log entry.
fn log_entry(labels: &str, entry: &Value) -> ClientResult<Value> {
    let (timestamp, line, metadata) = match list(entry, "values")?.as_slice() {
        [Value::String(timestamp), Value::String(line)] => (timestamp, line, String::new()),
        [
            Value::String(timestamp),
            Value::String(line),
            metadata @ Value::Object(_),
        ] => (timestamp, line, metadata.to_string()),
        _ => {
            return Err(malformed(
                "a log entry is not [\"timestamp\", \"line\"] with optional metadata",
            ));
        }
    };
    Ok(json!({
        "labels": labels,
        "timestamp": timestamp,
        "line": line,
        "metadata": metadata,
    }))
}

fn series_rows(body: &Value) -> ClientResult<Vec<Value>> {
    if body.get("status").and_then(Value::as_str) != Some("success") {
        return Err(malformed("`status` is not \"success\""));
    }
    let data = field(body, "data", "the response")?;
    let kind = string(field(data, "resultType", "`data`")?, "resultType")?;
    let result = field(data, "result", "`data`")?;
    let mut rows = Vec::new();
    match kind {
        "vector" => {
            for item in list(result, "result")? {
                let series = labels(item, "metric")?;
                rows.push(sample(&series, field(item, "value", "a vector sample")?)?);
            }
        }
        "matrix" => {
            for item in list(result, "result")? {
                let series = labels(item, "metric")?;
                for point in list(field(item, "values", "a matrix series")?, "values")? {
                    rows.push(sample(&series, point)?);
                }
            }
        }
        "streams" => {
            for item in list(result, "result")? {
                let stream = labels(item, "stream")?;
                for entry in list(field(item, "values", "a stream")?, "values")? {
                    rows.push(log_entry(&stream, entry)?);
                }
            }
        }
        "scalar" => rows.push(sample("{}", result)?),
        "string" => {
            let (timestamp, value) = match list(result, "result")?.as_slice() {
                [timestamp @ Value::Number(_), Value::String(value)] => (timestamp, value),
                _ => return Err(malformed("a string result is not [timestamp, \"text\"]")),
            };
            rows.push(json!({
                "labels": "{}",
                "timestamp": timestamp.to_string(),
                "value": value,
            }));
        }
        other => {
            return Err(malformed(&format!("unknown result type {other:?}")));
        }
    }
    Ok(rows)
}

/// A Tempo search's traces. Every field the server defines must be present
/// with its type, so no cell is left blank by a missing field.
fn trace_rows(body: &Value) -> ClientResult<Vec<Value>> {
    let traces = list(field(body, "traces", "the response")?, "traces")?;
    let mut rows = Vec::with_capacity(traces.len());
    for trace in traces {
        let text = |key: &str| -> ClientResult<String> {
            Ok(string(field(trace, key, "a trace")?, key)?.to_string())
        };
        let trace_id = text("traceID")?;
        if trace_id.is_empty() {
            return Err(malformed("a trace has an empty `traceID`"));
        }
        let duration = field(trace, "durationMs", "a trace")?
            .as_u64()
            .ok_or_else(|| malformed("`durationMs` is not a whole number"))?;
        rows.push(json!({
            "traceID": trace_id,
            "rootServiceName": text("rootServiceName")?,
            "rootTraceName": text("rootTraceName")?,
            "startTimeUnixNano": text("startTimeUnixNano")?,
            "durationMs": duration.to_string(),
        }));
    }
    Ok(rows)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    fn success(data: Value) -> Value {
        json!({"status": "success", "data": data})
    }

    #[test]
    fn vectors_and_matrices_become_one_row_per_sample() {
        let vector = success(json!({"resultType": "vector", "result": [
            {"metric": {"job": "a", "__name__": "up"}, "value": [1700000000.5, "NaN"]}
        ]}));
        let rows = check(Rows::Series, &vector).expect("vector");
        assert_eq!(
            rows,
            [json!({
                "labels": "{__name__=\"up\", job=\"a\"}",
                "timestamp": "1700000000.5",
                "value": "NaN",
            })]
        );
        let matrix = success(json!({"resultType": "matrix", "result": [
            {"metric": {}, "values": [[1, "+Inf"], [2, "3"]]}
        ]}));
        let rows = check(Rows::Series, &matrix).expect("matrix");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["value"], "+Inf");
        assert_eq!(rows[1]["labels"], "{}");
        let empty = success(json!({"resultType": "vector", "result": []}));
        assert!(check(Rows::Series, &empty).expect("empty").is_empty());
    }

    #[test]
    fn streams_keep_nanosecond_timestamps_and_metadata() {
        let streams = success(json!({"resultType": "streams", "result": [
            {"stream": {"app": "x"}, "values": [
                ["1700000000123456789", "hello", {"structuredMetadata": {"trace_id": "t"}}],
                ["1700000000123456790", "world"]
            ]}
        ]}));
        let rows = check(Rows::Series, &streams).expect("streams");
        assert_eq!(rows[0]["timestamp"], "1700000000123456789");
        assert_eq!(rows[0]["line"], "hello");
        assert_eq!(
            rows[0]["metadata"],
            "{\"structuredMetadata\":{\"trace_id\":\"t\"}}"
        );
        assert_eq!(rows[1]["metadata"], "");
    }

    #[test]
    fn scalars_strings_traces_and_trace_lookups_check() {
        let scalar = success(json!({"resultType": "scalar", "result": [1, "2"]}));
        assert_eq!(
            check(Rows::Series, &scalar).expect("scalar")[0]["value"],
            "2"
        );
        let text = success(json!({"resultType": "string", "result": [1, "hi"]}));
        assert_eq!(
            check(Rows::Series, &text).expect("string")[0]["value"],
            "hi"
        );
        let search = json!({"traces": [{"traceID": "ab", "rootServiceName": "api",
            "rootTraceName": "GET", "startTimeUnixNano": "17", "durationMs": 5,
            "spanSets": []}], "metrics": {}});
        let rows = check(Rows::Traces, &search).expect("traces");
        assert_eq!(rows[0]["traceID"], "ab");
        assert_eq!(rows[0]["durationMs"], "5");
        let trace = json!({"trace": {"resourceSpans": []}, "metrics": {}});
        assert!(check(Rows::None, &trace).expect("trace").is_empty());
    }

    #[test]
    fn malformed_shapes_are_protocol_errors_not_blank_output() {
        let trace = |change: fn(&mut Value)| {
            let mut search = json!({"traces": [{"traceID": "ab", "rootServiceName": "api",
                "rootTraceName": "GET", "startTimeUnixNano": "17", "durationMs": 5}]});
            change(&mut search["traces"][0]);
            search
        };
        let cases = [
            (Rows::Series, json!({})),
            (
                Rows::Series,
                json!({"data": {"resultType": "vector", "result": []}}),
            ),
            (
                Rows::Series,
                json!({"status": "error", "data": {"resultType": "vector", "result": []}}),
            ),
            (Rows::Series, success(json!({}))),
            (Rows::Series, success(json!({"resultType": "vector"}))),
            (
                Rows::Series,
                success(json!({"resultType": "vector", "result": {}})),
            ),
            (
                Rows::Series,
                success(json!({"resultType": "histogram", "result": []})),
            ),
            (
                Rows::Series,
                success(json!({"resultType": "vector", "result": [{"value": [1, "2"]}]})),
            ),
            (
                Rows::Series,
                success(json!({"resultType": "vector", "result": [{"metric": {}}]})),
            ),
            (
                Rows::Series,
                success(json!({"resultType": "vector", "result": [
                    {"metric": {"job": 1}, "value": [1, "2"]}
                ]})),
            ),
            (
                Rows::Series,
                success(json!({"resultType": "vector", "result": [
                    {"metric": {}, "value": [1]}
                ]})),
            ),
            (
                Rows::Series,
                success(json!({"resultType": "vector", "result": [
                    {"metric": {}, "value": ["1", 2]}
                ]})),
            ),
            (
                Rows::Series,
                success(json!({"resultType": "matrix", "result": [{"metric": {}}]})),
            ),
            (
                Rows::Series,
                success(json!({"resultType": "streams", "result": [
                    {"stream": {}, "values": [["1"]]}
                ]})),
            ),
            (
                Rows::Series,
                success(json!({"resultType": "streams", "result": [
                    {"stream": {}, "values": [[1, "line"]]}
                ]})),
            ),
            (
                Rows::Series,
                success(json!({"resultType": "streams", "result": [
                    {"stream": {}, "values": [["1", "line", "not an object"]]}
                ]})),
            ),
            (
                Rows::Series,
                success(json!({"resultType": "scalar", "result": []})),
            ),
            (
                Rows::Series,
                success(json!({"resultType": "string", "result": [1, 2]})),
            ),
            (Rows::Traces, json!({})),
            (Rows::Traces, json!({"traces": "x"})),
            (Rows::Traces, trace(|t| t["traceID"] = json!(""))),
            (Rows::Traces, trace(|t| t["rootServiceName"] = Value::Null)),
            (
                Rows::Traces,
                trace(|t| {
                    t.as_object_mut().expect("object").remove("rootTraceName");
                }),
            ),
            (Rows::Traces, trace(|t| t["startTimeUnixNano"] = json!(17))),
            (Rows::Traces, trace(|t| t["durationMs"] = json!("5"))),
            (Rows::Traces, trace(|t| t["durationMs"] = json!(-1))),
            (Rows::None, json!({})),
            (Rows::None, json!({"trace": []})),
        ];
        for (rows, body) in cases {
            match check(rows, &body) {
                Err(ClientError::ProtocolError(message)) => {
                    assert!(message.starts_with("malformed response"), "{message}");
                }
                other => panic!("{rows:?} {body}: {other:?}"),
            }
        }
    }

    #[test]
    fn server_warnings_and_infos_are_read_as_text_lists() {
        let body = json!({"status": "success", "warnings": ["w1", "w2"], "infos": ["i1"]});
        assert_eq!(
            notices(&body).expect("notices"),
            ServerNotices {
                warnings: vec!["w1".into(), "w2".into()],
                infos: vec!["i1".into()],
            }
        );
        assert_eq!(
            notices(&json!({"warnings": null})).expect("null"),
            ServerNotices::default()
        );
        for bad in [
            json!({"warnings": "w"}),
            json!({"warnings": [1]}),
            json!({"infos": {"a": "b"}}),
        ] {
            assert!(
                matches!(notices(&bad), Err(ClientError::ProtocolError(_))),
                "{bad}"
            );
        }
    }
}
