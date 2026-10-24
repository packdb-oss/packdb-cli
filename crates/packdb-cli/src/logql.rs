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

//! `packdb query logql`.

use packdb_client::{Direction, LogOptions};

use crate::Io;
use crate::api::{self, Plan, Request, Rows};
use crate::connection::{ConnectionArgs, UsageError};
use crate::exit::Exit;
use crate::output::QueryFormat;
use crate::time::{Timestamp, Window, duration_text, parse_duration, parse_time};

const UNKNOWN_REASON: &str =
    "the server's LogQL response is not established to mark every incomplete result";

/// Order of log lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum LogDirection {
    /// Oldest first.
    Forward,
    /// Newest first.
    Backward,
}

/// Arguments of `packdb query logql`.
///
/// Queries a range by default: the hour before now. With `--time` a metric
/// expression is evaluated at one instant instead; log queries need a range.
/// Times are RFC 3339 in UTC, Unix seconds, `now` or `now-<duration>`.
#[derive(Debug, Clone, clap::Args)]
pub struct LogqlArgs {
    /// The LogQL expression.
    pub expr: String,

    /// Where to connect and how to authenticate.
    #[command(flatten)]
    pub connection: ConnectionArgs,

    /// Evaluate a metric expression at one instant
    #[arg(long, value_name = "TIME", conflicts_with_all = ["start", "end", "step"])]
    pub time: Option<String>,

    /// Range start [default: one hour before --end]
    #[arg(long, value_name = "TIME")]
    pub start: Option<String>,

    /// Range end [default: now]
    #[arg(long, value_name = "TIME")]
    pub end: Option<String>,

    /// Resolution of a metric range query, such as 30s [default: the server's]
    #[arg(long, value_name = "DURATION")]
    pub step: Option<String>,

    /// Maximum number of log lines [default: the server's]
    #[arg(long, value_name = "N")]
    pub limit: Option<u32>,

    /// Order of log lines [default: backward]
    #[arg(long, value_enum)]
    pub direction: Option<LogDirection>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = QueryFormat::Table)]
    pub format: QueryFormat,
}

impl LogqlArgs {
    fn plan(&self, now: Timestamp) -> Result<Plan, UsageError> {
        let options = LogOptions {
            limit: self.limit,
            direction: self.direction.map(|direction| match direction {
                LogDirection::Forward => Direction::Forward,
                LogDirection::Backward => Direction::Backward,
            }),
        };
        let request = match &self.time {
            Some(text) => Request::LogqlInstant {
                query: self.expr.clone(),
                time: Some(parse_time(text, "--time", now)?.nanos()),
                options,
            },
            None => {
                let window = Window::resolve(self.start.as_deref(), self.end.as_deref(), now)?;
                let step = match &self.step {
                    Some(text) => Some(duration_text(parse_duration(text, "--step")?)),
                    None => None,
                };
                Request::LogqlRange {
                    query: self.expr.clone(),
                    start: window.start.nanos(),
                    end: window.end.nanos(),
                    step,
                    options,
                }
            }
        };
        Ok(Plan {
            command: "query logql",
            request,
            rows: Rows::Series,
            absent_means_complete: false,
            unknown_reason: UNKNOWN_REASON,
        })
    }
}

pub(crate) async fn run(args: LogqlArgs, io: Io<'_>) -> Exit {
    let plan = args.plan(Timestamp::now());
    api::run(&args.connection, args.format, plan, io).await
}
