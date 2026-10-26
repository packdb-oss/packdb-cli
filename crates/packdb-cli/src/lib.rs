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

//! The command library behind `packdb`.
//!
//! [`Command`] is a clap subcommand set; [`run`] executes one against a running
//! PackDB server and returns its [`Exit`] status. The library ships no
//! executable: the `packdb` binary that embeds it is assembled with the server.
//! `COMPATIBILITY.md` is the supported contract: arguments, environment,
//! output and exit codes.

#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]
#![deny(clippy::indexing_slicing)]
#![deny(clippy::panic)]
#![deny(clippy::unreachable)]

mod api;
mod bundle;
mod connection;
mod doctor;
mod environment;
mod exit;
mod logql;
mod output;
mod probe;
mod promql;
mod render;
mod report;
mod secrets;
mod smoke;
mod sql;
mod time;
mod trace;
mod traceql;
mod usage;

use std::io::Write;

pub use bundle::{BUNDLE_FORMAT, SupportBundleArgs};
pub use connection::{ConnectionArgs, DEFAULT_ENDPOINT};
pub use doctor::DoctorArgs;
pub use environment::{Environment, ProcessEnvironment};
pub use exit::Exit;
pub use logql::{LogDirection, LogqlArgs};
pub use output::{OperatorFormat, QueryFormat, SCHEMA_VERSION};
pub use promql::PromqlArgs;
pub use report::REPORT_SCHEMA_VERSION;
pub use smoke::{FIXTURE_VERSION, SmokeTestArgs};
pub use sql::{SqlArgs, TransportChoice};
pub use trace::TraceArgs;
pub use traceql::TraceqlArgs;
pub use usage::UsageReportArgs;

/// A `packdb` client command.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum Command {
    /// Run a query against a running PackDB server.
    Query {
        /// The query language.
        #[command(subcommand)]
        language: QueryCommand,
    },
    /// Read one trace by its ID.
    Trace(TraceArgs),
    /// Check a running server through its HTTP surface.
    Doctor(DoctorArgs),
    /// Write a small fixture and read this run's data back on every surface.
    SmokeTest(SmokeTestArgs),
    /// Collect diagnostics into one archive for support, without credentials.
    SupportBundle(SupportBundleArgs),
    /// Report the server's resource usage for its operator, each value with its scope and period.
    UsageReport(UsageReportArgs),
}

/// A query language.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum QueryCommand {
    /// Run a SQL statement.
    Sql(SqlArgs),
    /// Evaluate a PromQL expression at an instant or over a range.
    Promql(PromqlArgs),
    /// Run a LogQL query over a range, or a metric expression at an instant.
    Logql(LogqlArgs),
    /// Search traces with TraceQL.
    Traceql(TraceqlArgs),
}

/// Where a command writes its output, and what it reads its settings from.
pub struct Io<'a> {
    /// The result, and nothing else.
    pub stdout: &'a mut dyn Write,
    /// Warnings, notes and errors.
    pub stderr: &'a mut dyn Write,
    /// Environment variables and credential files.
    pub env: &'a dyn Environment,
}

/// Run a command and return its exit status.
pub async fn run(command: Command, io: Io<'_>) -> Exit {
    match command {
        Command::Query { language } => match language {
            QueryCommand::Sql(args) => sql::run(args, io).await,
            QueryCommand::Promql(args) => promql::run(args, io).await,
            QueryCommand::Logql(args) => logql::run(args, io).await,
            QueryCommand::Traceql(args) => traceql::run(args, io).await,
        },
        Command::Trace(args) => trace::run(args, io).await,
        Command::Doctor(args) => doctor::run(args, io).await,
        Command::SmokeTest(args) => smoke::run(args, io).await,
        Command::SupportBundle(args) => bundle::run(args, io).await,
        Command::UsageReport(args) => usage::run(args, io).await,
    }
}
