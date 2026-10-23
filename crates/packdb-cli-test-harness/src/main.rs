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

//! Runs the `packdb-cli` command library as a process, so tests can assert
//! real stdout, stderr and exit codes. Test-only: it is never released, and
//! it is not the `packdb` binary, which is assembled with the server.

use std::process::ExitCode;

use clap::Parser;

#[derive(Parser)]
#[command(
    name = "packdb",
    version,
    about = "PackDB client commands (test harness)",
    arg_required_else_help = true,
    subcommand_required = true
)]
struct Cli {
    #[command(subcommand)]
    command: packdb_cli::Command,
}

fn main() -> ExitCode {
    // clap prints help and exits 2 when no subcommand is given, exits 2 on any
    // other usage error, and exits 0 for --help and --version.
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("error: cannot start the runtime: {error}");
            return ExitCode::from(packdb_cli::Exit::Usage.code());
        }
    };
    let mut stdout = std::io::stdout().lock();
    let mut stderr = std::io::stderr().lock();
    let exit = runtime.block_on(packdb_cli::run(
        cli.command,
        packdb_cli::Io {
            stdout: &mut stdout,
            stderr: &mut stderr,
            env: &packdb_cli::ProcessEnvironment,
        },
    ));
    ExitCode::from(exit.code())
}
