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

//! Buffered output and its delivery, shared by every command.

use std::io::Write;

use crate::exit::Exit;

/// stdout and stderr text, written only once a command has decided its exit.
#[derive(Default)]
pub(crate) struct Rendered {
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

impl Rendered {
    pub(crate) fn error(message: &str) -> Self {
        let mut out = Rendered::default();
        Rendered::line(&mut out.stderr, &format!("error: {message}"));
        out
    }

    pub(crate) fn line(text: &mut String, line: &str) {
        text.push_str(line);
        if !line.ends_with('\n') {
            text.push('\n');
        }
    }
}

/// Write the rendered streams and flush them. A result that cannot be
/// delivered is a failure of its own, [`Exit::OutputFailed`], reported on
/// stderr while stderr still works.
pub(crate) fn deliver(
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    out: &Rendered,
    exit: Exit,
) -> Exit {
    let written = stdout
        .write_all(out.stdout.as_bytes())
        .and_then(|()| stdout.flush());
    let reported = stderr
        .write_all(out.stderr.as_bytes())
        .and_then(|()| stderr.flush());
    match (written, reported) {
        (Ok(()), Ok(())) => exit,
        (Err(error), Ok(())) => {
            // Best effort: if this report cannot be written either, the exit
            // code still says the output failed.
            let report = writeln!(stderr, "error: cannot write the result to stdout: {error}")
                .and_then(|()| stderr.flush());
            drop(report);
            Exit::OutputFailed
        }
        (_, Err(_)) => Exit::OutputFailed,
    }
}
