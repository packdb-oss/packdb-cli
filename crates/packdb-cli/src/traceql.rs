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

//! `packdb query traceql`.

use crate::Io;
use crate::api::{self, Plan, Request, Rows};
use crate::connection::{ConnectionArgs, UsageError};
use crate::exit::Exit;
use crate::output::QueryFormat;
use crate::time::{Timestamp, Window};

const UNKNOWN_REASON: &str =
    "a trace search stops at its limits without marking the result incomplete";

/// Arguments of `packdb query traceql`: a trace search over a window, by
/// default the hour before now. Times are RFC 3339 in UTC, Unix seconds, `now`
/// or `now-<duration>`; the window is widened to whole seconds.
#[derive(Debug, Clone, clap::Args)]
pub struct TraceqlArgs {
    /// The TraceQL query.
    pub query: String,

    /// Where to connect and how to authenticate.
    #[command(flatten)]
    pub connection: ConnectionArgs,

    /// Search start [default: one hour before --end]
    #[arg(long, value_name = "TIME")]
    pub start: Option<String>,

    /// Search end [default: now]
    #[arg(long, value_name = "TIME")]
    pub end: Option<String>,

    /// Maximum number of traces [default: the server's]
    #[arg(long, value_name = "N")]
    pub limit: Option<u32>,

    /// Output format.
    #[arg(long, value_enum, default_value_t = QueryFormat::Table)]
    pub format: QueryFormat,
}

impl TraceqlArgs {
    fn plan(&self, now: Timestamp) -> Result<Plan, UsageError> {
        let window = Window::resolve(self.start.as_deref(), self.end.as_deref(), now)?;
        Ok(Plan {
            command: "query traceql",
            request: Request::TraceqlSearch {
                query: self.query.clone(),
                start: window.start.floor_seconds("--start")?,
                end: window.end.ceil_seconds("--end")?,
                limit: self.limit,
            },
            rows: Rows::Traces,
            absent_means_complete: false,
            unknown_reason: UNKNOWN_REASON,
        })
    }
}

pub(crate) async fn run(args: TraceqlArgs, io: Io<'_>) -> Exit {
    let plan = args.plan(Timestamp::now());
    api::run(&args.connection, args.format, plan, io).await
}
