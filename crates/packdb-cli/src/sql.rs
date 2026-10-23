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

//! `packdb query sql`.

use http::{HeaderMap, HeaderValue};
use packdb_client::{
    ClientError, OutputFormat, RowsAnswer, SqlClient, SqlClientConfig, SqlTransport, TransportUsed,
};
use serde_json::{Value, json};

use crate::Io;
use crate::connection::ConnectionArgs;
use crate::exit::Exit;
use crate::output::{
    Completeness, Envelope, ErrorReport, QueryFormat, TransportReport, completeness_warning,
};

/// Whether an absent completeness carrier on the SQL path establishes a
/// complete result. Not yet: the server's SQL path has not been verified to
/// mark every incomplete read, so an absent carrier is reported as unknown.
const SQL_ABSENT_CARRIER_MEANS_COMPLETE: bool = false;

const UNKNOWN_REASON: &str =
    "the server's SQL response is not established to mark every incomplete result";

/// Which transport `packdb query sql` uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum TransportChoice {
    /// Flight SQL when an endpoint is configured, otherwise HTTP. Falls back to
    /// HTTP once, only when the Flight SQL connection could not be opened before
    /// the query was sent.
    #[default]
    Auto,
    /// HTTP only.
    Http,
    /// Flight SQL only.
    FlightSql,
}

impl TransportChoice {
    fn as_str(self) -> &'static str {
        match self {
            TransportChoice::Auto => "auto",
            TransportChoice::Http => "http",
            TransportChoice::FlightSql => "flight-sql",
        }
    }

    fn policy(self) -> SqlTransport {
        match self {
            TransportChoice::Auto => SqlTransport::Auto,
            TransportChoice::Http => SqlTransport::Http,
            TransportChoice::FlightSql => SqlTransport::FlightSql,
        }
    }
}

/// Arguments of `packdb query sql`.
#[derive(Debug, Clone, clap::Args)]
pub struct SqlArgs {
    /// The SQL statement.
    pub sql: String,

    /// Where to connect and how to authenticate.
    #[command(flatten)]
    pub connection: ConnectionArgs,

    /// Transport.
    #[arg(long, value_enum, default_value_t = TransportChoice::Auto)]
    pub transport: TransportChoice,

    /// Output format.
    #[arg(long, value_enum, default_value_t = QueryFormat::Table)]
    pub format: QueryFormat,
}

fn used(transport: TransportUsed) -> &'static str {
    match transport {
        TransportUsed::Http => "http",
        TransportUsed::FlightSql => "flight-sql",
    }
}

fn error_kind(error: &ClientError) -> &'static str {
    match error {
        ClientError::SqlError(_) => "query_rejected",
        ClientError::Unauthenticated(_) => "unauthenticated",
        ClientError::PermissionDenied(_) => "permission_denied",
        ClientError::ConnectionError(_) => "unreachable",
        ClientError::ConfigError(_) | ClientError::IoError(_) => "configuration",
        _ => "server_or_protocol",
    }
}

pub(crate) async fn run(args: SqlArgs, io: Io<'_>) -> Exit {
    let Io {
        stdout,
        stderr,
        env,
    } = io;
    let connection = match args.connection.resolve(env) {
        Ok(connection) => connection,
        Err(error) => {
            let _ = writeln!(stderr, "error: {}", error.0);
            return Exit::Usage;
        }
    };
    let mut headers = HeaderMap::new();
    if let Some(tenant) = &connection.tenant {
        match HeaderValue::from_str(tenant) {
            Ok(value) => {
                headers.insert("x-scope-orgid", value);
            }
            Err(_) => {
                let _ = writeln!(stderr, "error: the tenant is not a valid header value");
                return Exit::Usage;
            }
        }
    }
    let endpoint = connection.endpoint.clone();
    let client = SqlClient::new(SqlClientConfig {
        url: connection.endpoint,
        flight_url: connection.flight_endpoint,
        auth: connection.auth.into_provider(),
        timeout: connection.timeout,
        transport: args.transport.policy(),
    });
    let result = client
        .execute_query_rows_with_transport(&args.sql, headers)
        .await;
    let mut out = Rendered::default();
    let exit = match result {
        Ok(answer) => render_answer(&args, endpoint, answer, &mut out),
        Err(error) => render_error(&args, endpoint, &error, &mut out),
    };
    let _ = stdout.write_all(out.stdout.as_bytes());
    let _ = stderr.write_all(out.stderr.as_bytes());
    exit
}

#[derive(Default)]
struct Rendered {
    stdout: String,
    stderr: String,
}

impl Rendered {
    fn line(text: &mut String, line: &str) {
        text.push_str(line);
        if !line.ends_with('\n') {
            text.push('\n');
        }
    }
}

fn render_answer(args: &SqlArgs, endpoint: String, answer: RowsAnswer, out: &mut Rendered) -> Exit {
    let RowsAnswer {
        response,
        transport,
        fallback_reason,
    } = answer;
    let completeness =
        Completeness::of(&response.partial_causes, SQL_ABSENT_CARRIER_MEANS_COMPLETE);
    let mut warnings = Vec::new();
    if let Some(reason) = &fallback_reason {
        warnings.push(format!(
            "Flight SQL could not be reached before the query was sent; answered over HTTP ({reason})"
        ));
    }
    if let Some(warning) =
        completeness_warning(completeness, &response.partial_causes, UNKNOWN_REASON)
    {
        warnings.push(warning);
    }
    let status = if completeness == Completeness::Complete {
        Exit::Success
    } else {
        Exit::Incomplete
    };
    let rendered = match args.format {
        QueryFormat::Json => Ok(Envelope {
            command: "query sql",
            endpoint,
            transport: TransportReport {
                requested: args.transport.as_str(),
                used: Some(used(transport)),
                fallback_reason,
            },
            completeness: Some(completeness),
            partial_causes: response.partial_causes,
            warnings: warnings.clone(),
            error: None,
            data: json!({ "rows": response.data }),
        }
        .to_json()
        .to_string()),
        QueryFormat::Table => OutputFormat::Table.render(&response.data),
        QueryFormat::Csv => OutputFormat::Csv.render(&response.data),
    };
    for warning in &warnings {
        Rendered::line(&mut out.stderr, &format!("warning: {warning}"));
    }
    match rendered {
        Ok(text) => {
            Rendered::line(&mut out.stdout, &text);
            status
        }
        Err(error) => {
            Rendered::line(&mut out.stderr, &format!("error: {error}"));
            Exit::of(&error)
        }
    }
}

fn render_error(args: &SqlArgs, endpoint: String, error: &ClientError, out: &mut Rendered) -> Exit {
    if args.format == QueryFormat::Json {
        let envelope = Envelope {
            command: "query sql",
            endpoint,
            transport: TransportReport {
                requested: args.transport.as_str(),
                used: None,
                fallback_reason: None,
            },
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

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn transport_choices_name_and_map_to_the_sdk_policy() {
        let cases = [
            (TransportChoice::Auto, "auto", SqlTransport::Auto),
            (TransportChoice::Http, "http", SqlTransport::Http),
            (
                TransportChoice::FlightSql,
                "flight-sql",
                SqlTransport::FlightSql,
            ),
        ];
        for (choice, name, policy) in cases {
            assert_eq!(choice.as_str(), name);
            assert_eq!(choice.policy(), policy);
        }
        assert_eq!(used(TransportUsed::Http), "http");
        assert_eq!(used(TransportUsed::FlightSql), "flight-sql");
    }

    #[test]
    fn error_kinds_name_what_failed() {
        let cases = [
            (ClientError::SqlError("x".into()), "query_rejected"),
            (ClientError::Unauthenticated("x".into()), "unauthenticated"),
            (
                ClientError::PermissionDenied("x".into()),
                "permission_denied",
            ),
            (ClientError::ConnectionError("x".into()), "unreachable"),
            (ClientError::ConfigError("x".into()), "configuration"),
            (ClientError::ProtocolError("x".into()), "server_or_protocol"),
        ];
        for (error, kind) in cases {
            assert_eq!(error_kind(&error), kind, "{error:?}");
        }
    }
}
