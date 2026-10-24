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

//! `packdb trace`.

use crate::Io;
use crate::api::{self, Plan, Request, Rows};
use crate::connection::{ConnectionArgs, UsageError};
use crate::exit::Exit;
use crate::output::QueryFormat;
use crate::time::{Timestamp, Window};

/// Arguments of `packdb trace`: read one trace by its ID, as JSON. With
/// `--start` and `--end` only that window is searched; times are RFC 3339 in
/// UTC, Unix seconds, `now` or `now-<duration>`, widened to whole seconds.
#[derive(Debug, Clone, clap::Args)]
pub struct TraceArgs {
    /// The trace ID, in hexadecimal.
    pub trace_id: String,

    /// Where to connect and how to authenticate.
    #[command(flatten)]
    pub connection: ConnectionArgs,

    /// Window start; requires --end
    #[arg(long, value_name = "TIME", requires = "end")]
    pub start: Option<String>,

    /// Window end; requires --start
    #[arg(long, value_name = "TIME", requires = "start")]
    pub end: Option<String>,

    /// Output format. A trace has no table form.
    #[arg(long, value_enum, default_value_t = QueryFormat::Json)]
    pub format: QueryFormat,
}

impl TraceArgs {
    fn plan(&self, now: Timestamp) -> Result<Plan, UsageError> {
        let window = match (&self.start, &self.end) {
            (Some(start), Some(end)) => {
                let window = Window::resolve(Some(start), Some(end), now)?;
                Some((
                    window.start.floor_seconds("--start")?,
                    window.end.ceil_seconds("--end")?,
                ))
            }
            _ => None,
        };
        Ok(Plan {
            command: "trace",
            request: Request::Trace {
                id: self.trace_id.clone(),
                window,
            },
            rows: Rows::None,
            // Verified: a partial miss answers with the carrier and a complete
            // miss is a 404, so an absent carrier on a found trace is complete.
            absent_means_complete: true,
            unknown_reason: "",
        })
    }
}

pub(crate) async fn run(args: TraceArgs, io: Io<'_>) -> Exit {
    let plan = args.plan(Timestamp::now());
    api::run(&args.connection, args.format, plan, io).await
}
