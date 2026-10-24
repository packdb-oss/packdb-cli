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

//! `packdb query promql`.

use crate::Io;
use crate::api::{self, Plan, Request, Rows};
use crate::connection::{ConnectionArgs, UsageError};
use crate::exit::Exit;
use crate::output::QueryFormat;
use crate::time::{Timestamp, Window, duration_text, parse_duration, parse_time};

const UNKNOWN_REASON: &str =
    "the server's PromQL response is not established to mark every incomplete result";

/// Arguments of `packdb query promql`.
///
/// With `--start`, `--end` or `--step` the expression is evaluated over a range;
/// otherwise at one instant. Times are RFC 3339 in UTC, Unix seconds, `now` or
/// `now-<duration>`.
#[derive(Debug, Clone, clap::Args)]
pub struct PromqlArgs {
    /// The PromQL expression.
    pub expr: String,

    /// Where to connect and how to authenticate.
    #[command(flatten)]
    pub connection: ConnectionArgs,

    /// Evaluation time of an instant query [default: the server's current time]
    #[arg(long, value_name = "TIME", conflicts_with_all = ["start", "end", "step"])]
    pub time: Option<String>,

    /// Range start [default: one hour before --end]
    #[arg(long, value_name = "TIME")]
    pub start: Option<String>,

    /// Range end [default: now]
    #[arg(long, value_name = "TIME")]
    pub end: Option<String>,

    /// Range resolution, such as 15s [default: the range over 250 points, at least 1s]
    #[arg(long, value_name = "DURATION")]
    pub step: Option<String>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = QueryFormat::Table)]
    pub format: QueryFormat,
}

impl PromqlArgs {
    fn plan(&self, now: Timestamp) -> Result<Plan, UsageError> {
        let range = self.start.is_some() || self.end.is_some() || self.step.is_some();
        let request = if range {
            let window = Window::resolve(self.start.as_deref(), self.end.as_deref(), now)?;
            let step = match &self.step {
                Some(text) => parse_duration(text, "--step")?,
                None => window.default_step(),
            };
            Request::PromqlRange {
                query: self.expr.clone(),
                start: window.start.seconds(),
                end: window.end.seconds(),
                step: duration_text(step),
            }
        } else {
            let time = match &self.time {
                Some(text) => Some(parse_time(text, "--time", now)?.seconds()),
                None => None,
            };
            Request::PromqlInstant {
                query: self.expr.clone(),
                time,
            }
        };
        Ok(Plan {
            command: "query promql",
            request,
            rows: Rows::Series,
            absent_means_complete: false,
            unknown_reason: UNKNOWN_REASON,
        })
    }
}

pub(crate) async fn run(args: PromqlArgs, io: Io<'_>) -> Exit {
    let plan = args.plan(Timestamp::now());
    api::run(&args.connection, args.format, plan, io).await
}
