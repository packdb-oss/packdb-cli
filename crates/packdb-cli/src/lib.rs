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

mod connection;
mod environment;
mod exit;
mod output;
mod sql;

use std::io::Write;

pub use connection::{ConnectionArgs, DEFAULT_ENDPOINT};
pub use environment::{Environment, ProcessEnvironment};
pub use exit::Exit;
pub use output::{QueryFormat, SCHEMA_VERSION};
pub use sql::{SqlArgs, TransportChoice};

/// A `packdb` client command.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum Command {
    /// Run a query against a running PackDB server.
    Query {
        /// The query language.
        #[command(subcommand)]
        language: QueryCommand,
    },
}

/// A query language.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum QueryCommand {
    /// Run a SQL statement.
    Sql(SqlArgs),
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
        Command::Query {
            language: QueryCommand::Sql(args),
        } => sql::run(args, io).await,
    }
}
