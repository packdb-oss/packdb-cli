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

//! The output contract shared by query commands.

use serde_json::{Value, json};

/// The JSON schema version of every command's JSON output.
pub const SCHEMA_VERSION: &str = "packdb.cli.v1";

/// Output format of a query command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum QueryFormat {
    /// An aligned table.
    #[default]
    Table,
    /// One JSON object: the CLI envelope with the result under `data`.
    Json,
    /// Comma-separated values with a header row.
    Csv,
}

/// Output format of an operator command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum OperatorFormat {
    /// A human report.
    #[default]
    Text,
    /// One JSON object: the CLI envelope with the report under `data`.
    Json,
}

/// Whether a result is known to be complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Completeness {
    /// The protocol's documented contract establishes completeness, and no
    /// cause was reported.
    Complete,
    /// The server reported why the result is incomplete.
    Partial,
    /// Completeness cannot be established. Never reported as complete.
    Unknown,
}

impl Completeness {
    /// Classify a result. `absent_means_complete` is true only for a path whose
    /// documented and verified contract says an absent cause list means
    /// complete.
    pub(crate) fn of(causes: &[String], absent_means_complete: bool) -> Completeness {
        if !causes.is_empty() {
            Completeness::Partial
        } else if absent_means_complete {
            Completeness::Complete
        } else {
            Completeness::Unknown
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Completeness::Complete => "complete",
            Completeness::Partial => "partial",
            Completeness::Unknown => "unknown",
        }
    }
}

/// How the request travelled.
pub(crate) struct TransportReport {
    pub(crate) requested: &'static str,
    pub(crate) used: Option<&'static str>,
    pub(crate) fallback_reason: Option<String>,
}

/// A failed request, as reported in JSON.
pub(crate) struct ErrorReport {
    pub(crate) kind: &'static str,
    pub(crate) message: String,
}

/// The JSON envelope every command prints with `--format json`.
/// Notices the server attached to a successful answer: the `warnings` and
/// `infos` lists of a Prometheus or Loki response. Reported as received.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ServerNotices {
    pub(crate) warnings: Vec<String>,
    pub(crate) infos: Vec<String>,
}

pub(crate) struct Envelope {
    pub(crate) command: &'static str,
    pub(crate) endpoint: String,
    pub(crate) transport: TransportReport,
    pub(crate) completeness: Option<Completeness>,
    pub(crate) partial_causes: Vec<String>,
    pub(crate) warnings: Vec<String>,
    pub(crate) server: ServerNotices,
    pub(crate) error: Option<ErrorReport>,
    pub(crate) data: Value,
}

impl Envelope {
    pub(crate) fn to_json(&self) -> Value {
        json!({
            "schema_version": SCHEMA_VERSION,
            "command": self.command,
            "endpoint": display_endpoint(&self.endpoint),
            "transport": {
                "requested": self.transport.requested,
                "used": self.transport.used,
                "fallback_reason": self.transport.fallback_reason,
            },
            "completeness": self.completeness.map(Completeness::as_str),
            "partial_causes": self.partial_causes,
            "warnings": self.warnings,
            "server_warnings": self.server.warnings,
            "server_infos": self.server.infos,
            "error": self.error.as_ref().map(|error| json!({
                "kind": error.kind,
                "message": error.message,
            })),
            "data": self.data,
        })
    }
}

/// The endpoint without any user information it carries, so a credential
/// written into the URL is never printed.
pub(crate) fn display_endpoint(endpoint: &str) -> String {
    let Some((scheme, rest)) = endpoint.split_once("://") else {
        return endpoint.to_string();
    };
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    match authority.rsplit_once('@') {
        Some((_, host)) => format!("{scheme}://{host}{tail}"),
        None => endpoint.to_string(),
    }
}

/// The warning printed for a result that is not known to be complete.
pub(crate) fn completeness_warning(
    completeness: Completeness,
    causes: &[String],
    unknown_reason: &str,
) -> Option<String> {
    match completeness {
        Completeness::Complete => None,
        Completeness::Partial => Some(format!("incomplete result ({})", causes.join(", "))),
        Completeness::Unknown => Some(format!("completeness unknown ({unknown_reason})")),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn causes_make_a_result_partial_whatever_the_contract() {
        let causes = vec!["cold_tier_unavailable".to_string()];
        assert_eq!(Completeness::of(&causes, true), Completeness::Partial);
        assert_eq!(Completeness::of(&causes, false), Completeness::Partial);
    }

    #[test]
    fn no_causes_is_complete_only_where_the_contract_establishes_it() {
        assert_eq!(Completeness::of(&[], true), Completeness::Complete);
        assert_eq!(Completeness::of(&[], false), Completeness::Unknown);
    }

    #[test]
    fn warnings_name_the_causes_or_the_reason() {
        let causes = vec!["a".to_string(), "b".to_string()];
        assert_eq!(
            completeness_warning(Completeness::Partial, &causes, "why").as_deref(),
            Some("incomplete result (a, b)")
        );
        assert_eq!(
            completeness_warning(Completeness::Unknown, &[], "why").as_deref(),
            Some("completeness unknown (why)")
        );
        assert_eq!(
            completeness_warning(Completeness::Complete, &[], "why"),
            None
        );
    }

    #[test]
    fn user_information_is_removed_from_printed_endpoints() {
        let cases = [
            ("http://user:secret@host:4000/x?y", "http://host:4000/x?y"),
            ("http://host:4000", "http://host:4000"),
            ("https://a@b@host", "https://host"),
            ("not a url", "not a url"),
        ];
        for (endpoint, expected) in cases {
            assert_eq!(display_endpoint(endpoint), expected, "{endpoint}");
        }
    }

    #[test]
    fn the_envelope_carries_every_field() {
        let envelope = Envelope {
            command: "query sql",
            endpoint: "http://user:pw@host:4000".into(),
            transport: TransportReport {
                requested: "auto",
                used: Some("http"),
                fallback_reason: Some("refused".into()),
            },
            completeness: Some(Completeness::Partial),
            partial_causes: vec!["c".into()],
            warnings: vec!["w".into()],
            server: ServerNotices::default(),
            error: Some(ErrorReport {
                kind: "protocol",
                message: "m".into(),
            }),
            data: json!({"rows": []}),
        }
        .to_json();
        assert_eq!(envelope["schema_version"], SCHEMA_VERSION);
        assert_eq!(envelope["command"], "query sql");
        assert_eq!(envelope["endpoint"], "http://host:4000");
        assert_eq!(envelope["transport"]["requested"], "auto");
        assert_eq!(envelope["transport"]["used"], "http");
        assert_eq!(envelope["transport"]["fallback_reason"], "refused");
        assert_eq!(envelope["completeness"], "partial");
        assert_eq!(envelope["partial_causes"], json!(["c"]));
        assert_eq!(envelope["warnings"], json!(["w"]));
        assert_eq!(envelope["error"]["kind"], "protocol");
        assert_eq!(envelope["data"], json!({"rows": []}));
    }
}
