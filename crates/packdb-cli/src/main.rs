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

//! `packdb`, PackDB's command-line client. It parses a [`packdb_cli::Command`]
//! and exits with the status [`packdb_cli::run`] returns; the commands
//! themselves live in the library. It never starts a server: the single-node
//! server is `packdb-server`, built from the server repository.

#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]
#![deny(clippy::indexing_slicing)]
#![deny(clippy::panic)]
#![deny(clippy::unreachable)]

use std::io::{IsTerminal, Write};
use std::process::ExitCode;

use clap::Parser;
use packdb_cli::Exit;

#[derive(Parser)]
#[command(
    name = "packdb",
    version,
    about = "PackDB command-line client: queries and operator commands against a running PackDB server",
    arg_required_else_help = true,
    subcommand_required = true
)]
struct Cli {
    #[command(subcommand)]
    command: packdb_cli::Command,
}

fn main() -> ExitCode {
    let mut stdout = std::io::stdout().lock();
    let mut stderr = std::io::stderr().lock();
    // Parsing comes first, so help, version and usage errors never build a
    // runtime or open a connection. clap renders them; this process writes
    // them, so a failed write is reported like any other output failure.
    let exit = match Cli::try_parse() {
        Ok(cli) => execute(cli.command, &mut stdout, &mut stderr),
        Err(error) => deliver_parse_outcome(&error, &mut stdout, &mut stderr),
    };
    ExitCode::from(exit.code())
}

fn execute(command: packdb_cli::Command, stdout: &mut dyn Write, stderr: &mut dyn Write) -> Exit {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            return match writeln!(stderr, "error: cannot start the runtime: {error}")
                .and_then(|()| stderr.flush())
            {
                Ok(()) => Exit::Usage,
                Err(_) => Exit::OutputFailed,
            };
        }
    };
    runtime.block_on(packdb_cli::run(
        command,
        packdb_cli::Io {
            stdout,
            stderr,
            env: &packdb_cli::ProcessEnvironment,
        },
    ))
}

/// Write what clap rendered: `--help` and `--version` on stdout with `0`;
/// bare `packdb`'s help and every usage error on stderr with `2`. Output that
/// cannot be written or flushed exits `8`, as the contract promises.
fn deliver_parse_outcome(
    error: &clap::Error,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Exit {
    let exit = if error.exit_code() == 0 {
        Exit::Success
    } else {
        Exit::Usage
    };
    let on_stderr = error.use_stderr();
    let styled = if on_stderr {
        std::io::stderr().is_terminal()
    } else {
        std::io::stdout().is_terminal()
    };
    let rendered = error.render();
    let text = if styled {
        rendered.ansi().to_string()
    } else {
        rendered.to_string()
    };
    let stream: &mut dyn Write = if on_stderr { stderr } else { stdout };
    match stream
        .write_all(text.as_bytes())
        .and_then(|()| stream.flush())
    {
        Ok(()) => exit,
        Err(failure) => {
            if !on_stderr {
                // Best effort: if this report cannot be written either, the
                // exit code still says the output failed.
                drop(
                    writeln!(stderr, "error: cannot write to stdout: {failure}")
                        .and_then(|()| stderr.flush()),
                );
            }
            Exit::OutputFailed
        }
    }
}
