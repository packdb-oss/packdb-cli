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

//! The evidence report of the diagnostics commands.
//!
//! A report lists one finding per check and every piece of required evidence
//! that could not be collected. It keeps three outcomes apart: a check that
//! ran and passed or failed, a check whose evidence could not be collected
//! (`error`, which makes the report incomplete), and a check the server gives
//! no evidence for at all (`not_exposed`, which is reported and never counts
//! as a pass).

use std::collections::BTreeMap;
use std::time::SystemTime;

use serde_json::{Value, json};

use crate::exit::Exit;
use crate::output::Completeness;

/// The version of the report under the JSON envelope's `data`.
pub const REPORT_SCHEMA_VERSION: &str = "packdb.cli.diagnostics.v1";

/// The verdict of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Status {
    /// The condition holds.
    Pass,
    /// The condition holds, but something deserves attention.
    Warn,
    /// The condition does not hold.
    Fail,
    /// The evidence could not be collected; the verdict is unknown.
    Error,
    /// The server exposes no evidence for this check; the verdict is unknown.
    NotExposed,
}

impl Status {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Status::Pass => "pass",
            Status::Warn => "warn",
            Status::Fail => "fail",
            Status::Error => "error",
            Status::NotExposed => "not_exposed",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Status::Pass => "pass",
            Status::Warn => "warn",
            Status::Fail => "FAIL",
            Status::Error => "ERROR",
            Status::NotExposed => "not exposed",
        }
    }
}

/// How much a check matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Severity {
    /// Context only; never fails the command.
    Info,
    /// A required condition: a `fail` exits `1`.
    Required,
}

impl Severity {
    fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Required => "required",
        }
    }
}

/// One check's outcome.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Finding {
    pub(crate) id: &'static str,
    pub(crate) status: Status,
    pub(crate) severity: Severity,
    pub(crate) summary: String,
    pub(crate) evidence: BTreeMap<&'static str, Value>,
}

impl Finding {
    pub(crate) fn new(
        id: &'static str,
        severity: Severity,
        status: Status,
        summary: impl Into<String>,
    ) -> Finding {
        Finding {
            id,
            status,
            severity,
            summary: summary.into(),
            evidence: BTreeMap::new(),
        }
    }

    pub(crate) fn with(mut self, key: &'static str, value: impl Into<Value>) -> Finding {
        self.evidence.insert(key, value.into());
        self
    }

    fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "status": self.status.as_str(),
            "severity": self.severity.as_str(),
            "summary": self.summary,
            "evidence": self.evidence,
        })
    }
}

/// Required evidence that could not be collected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MissingEvidence {
    pub(crate) evidence: String,
    pub(crate) reason: String,
}

/// A diagnostics report, built as checks run.
#[derive(Debug)]
pub(crate) struct Report {
    pub(crate) command: &'static str,
    pub(crate) started_at: SystemTime,
    pub(crate) finished_at: SystemTime,
    pub(crate) target: BTreeMap<&'static str, Value>,
    pub(crate) scope: BTreeMap<&'static str, Value>,
    pub(crate) findings: Vec<Finding>,
    pub(crate) missing_evidence: Vec<MissingEvidence>,
}

impl Report {
    pub(crate) fn new(command: &'static str) -> Report {
        let now = SystemTime::now();
        Report {
            command,
            started_at: now,
            finished_at: now,
            target: BTreeMap::new(),
            scope: BTreeMap::new(),
            findings: Vec::new(),
            missing_evidence: Vec::new(),
        }
    }

    pub(crate) fn push(&mut self, finding: Finding) {
        self.findings.push(finding);
    }

    /// Record a check whose evidence could not be collected: an `error`
    /// finding, and the evidence it lacked. Either makes the report incomplete.
    pub(crate) fn uncollected(
        &mut self,
        id: &'static str,
        severity: Severity,
        evidence: &str,
        reason: &str,
    ) {
        self.findings.push(Finding::new(
            id,
            severity,
            Status::Error,
            format!("{evidence} could not be collected: {reason}"),
        ));
        self.missing_evidence.push(MissingEvidence {
            evidence: evidence.to_string(),
            reason: reason.to_string(),
        });
    }

    /// Whether every piece of required evidence was collected.
    pub(crate) fn is_complete(&self) -> bool {
        self.missing_evidence.is_empty()
            && !self
                .findings
                .iter()
                .any(|finding| finding.status == Status::Error)
    }

    pub(crate) fn completeness(&self) -> Completeness {
        if self.is_complete() {
            Completeness::Complete
        } else {
            Completeness::Unknown
        }
    }

    /// The exit the findings imply: a required check that demonstrably failed
    /// is `1` even when other evidence is missing; otherwise missing evidence
    /// is `3`.
    pub(crate) fn exit(&self) -> Exit {
        let failed = self.findings.iter().any(|finding| {
            finding.severity == Severity::Required && finding.status == Status::Fail
        });
        if failed {
            Exit::Rejected
        } else if !self.is_complete() {
            Exit::Incomplete
        } else {
            Exit::Success
        }
    }

    fn counts(&self) -> BTreeMap<&'static str, usize> {
        let mut counts = BTreeMap::new();
        for status in [
            Status::Pass,
            Status::Warn,
            Status::Fail,
            Status::Error,
            Status::NotExposed,
        ] {
            counts.insert(status.as_str(), 0);
        }
        for finding in &self.findings {
            *counts.entry(finding.status.as_str()).or_insert(0) += 1;
        }
        counts
    }

    pub(crate) fn to_json(&self) -> Value {
        json!({
            "schema_version": REPORT_SCHEMA_VERSION,
            "report_kind": self.command,
            "started_at": humantime::format_rfc3339_millis(self.started_at).to_string(),
            "finished_at": humantime::format_rfc3339_millis(self.finished_at).to_string(),
            "tool": {"name": "packdb-cli", "version": env!("CARGO_PKG_VERSION")},
            "target": self.target,
            "requested_scope": self.scope,
            "completeness": if self.is_complete() { "complete" } else { "incomplete" },
            "counts": self.counts(),
            "findings": self.findings.iter().map(Finding::to_json).collect::<Vec<_>>(),
            "missing_evidence": self.missing_evidence.iter().map(|missing| json!({
                "evidence": missing.evidence,
                "reason": missing.reason,
            })).collect::<Vec<_>>(),
        })
    }

    /// The human report: one line per finding, then the totals.
    pub(crate) fn to_text(&self) -> String {
        let width = self
            .findings
            .iter()
            .map(|finding| finding.id.len())
            .max()
            .unwrap_or(0);
        let mut text = String::new();
        for finding in &self.findings {
            text.push_str(&format!(
                "{:<11}  {:<width$}  {}\n",
                finding.status.label(),
                finding.id,
                finding.summary
            ));
        }
        let counts = self.counts();
        let count = |status: Status| counts.get(status.as_str()).copied().unwrap_or(0);
        text.push_str(&format!(
            "completeness: {}; {} pass, {} warn, {} fail, {} error, {} not exposed\n",
            if self.is_complete() {
                "complete"
            } else {
                "incomplete"
            },
            count(Status::Pass),
            count(Status::Warn),
            count(Status::Fail),
            count(Status::Error),
            count(Status::NotExposed),
        ));
        text
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn report(findings: Vec<Finding>) -> Report {
        let mut report = Report::new("doctor");
        for finding in findings {
            report.push(finding);
        }
        report
    }

    #[test]
    fn a_required_failure_exits_1_even_when_evidence_is_missing() {
        let mut failing = report(vec![Finding::new(
            "a",
            Severity::Required,
            Status::Fail,
            "no",
        )]);
        assert_eq!(failing.exit(), Exit::Rejected);
        failing.uncollected("b", Severity::Required, "b evidence", "timed out");
        assert_eq!(failing.exit(), Exit::Rejected);
        assert_eq!(failing.completeness(), Completeness::Unknown);
    }

    #[test]
    fn missing_evidence_exits_3_and_is_never_complete() {
        let mut missing = report(vec![Finding::new(
            "a",
            Severity::Required,
            Status::Pass,
            "ok",
        )]);
        assert_eq!(missing.exit(), Exit::Success);
        missing.uncollected("b", Severity::Required, "b evidence", "refused");
        assert_eq!(missing.exit(), Exit::Incomplete);
        assert!(!missing.is_complete());
        assert_eq!(missing.to_json()["completeness"], "incomplete");
        assert_eq!(
            missing.to_json()["missing_evidence"][0]["reason"],
            "refused"
        );
    }

    #[test]
    fn not_exposed_info_and_warn_findings_never_fail_the_command() {
        let report = report(vec![
            Finding::new("a", Severity::Info, Status::NotExposed, "not exposed"),
            Finding::new("b", Severity::Info, Status::Fail, "context"),
            Finding::new("c", Severity::Required, Status::Warn, "degraded"),
        ]);
        assert_eq!(report.exit(), Exit::Success);
        assert!(report.is_complete());
        let json = report.to_json();
        assert_eq!(json["counts"]["not_exposed"], 1);
        assert_eq!(json["findings"][0]["status"], "not_exposed");
        assert_eq!(json["schema_version"], REPORT_SCHEMA_VERSION);
    }

    #[test]
    fn text_lists_each_finding_and_the_totals() {
        let text = report(vec![
            Finding::new(
                "process.health",
                Severity::Required,
                Status::Pass,
                "healthy",
            ),
            Finding::new(
                "storage.tiers",
                Severity::Info,
                Status::NotExposed,
                "not exposed",
            ),
        ])
        .to_text();
        assert!(
            text.contains("pass         process.health  healthy\n"),
            "{text}"
        );
        assert!(text.contains("not exposed  storage.tiers"), "{text}");
        assert!(
            text.ends_with("1 pass, 0 warn, 0 fail, 0 error, 1 not exposed\n"),
            "{text}"
        );
    }
}
