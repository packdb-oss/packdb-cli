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

//! Where commands read environment variables and credential files.

use std::io::Read;
use std::path::Path;

/// Environment variables and files a command reads its settings from.
///
/// [`ProcessEnvironment`] reads the running process's. Tests supply their own,
/// so precedence and conflicts are asserted without changing the process.
pub trait Environment {
    /// The value of an environment variable, if it is set and valid Unicode.
    fn var(&self, name: &str) -> Option<String>;

    /// At most `limit + 1` bytes of a file, so a caller can tell a file that is
    /// too large from one that fits without reading all of it.
    ///
    /// # Errors
    ///
    /// Returns the I/O error if the file cannot be opened or read.
    fn read_file(&self, path: &Path, limit: u64) -> std::io::Result<Vec<u8>>;
}

/// The running process's environment and file system.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessEnvironment;

impl Environment for ProcessEnvironment {
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }

    fn read_file(&self, path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(limit.saturating_add(1))
            .read_to_end(&mut bytes)?;
        Ok(bytes)
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! An in-memory [`Environment`] for tests.

    use super::Environment;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    /// Variables and files held in memory.
    #[derive(Default)]
    pub(crate) struct FakeEnvironment {
        vars: HashMap<String, String>,
        files: HashMap<PathBuf, Vec<u8>>,
    }

    impl FakeEnvironment {
        pub(crate) fn var(mut self, name: &str, value: &str) -> Self {
            self.vars.insert(name.into(), value.into());
            self
        }

        pub(crate) fn file(mut self, path: &str, contents: &[u8]) -> Self {
            self.files.insert(path.into(), contents.to_vec());
            self
        }
    }

    impl Environment for FakeEnvironment {
        fn var(&self, name: &str) -> Option<String> {
            self.vars.get(name).cloned()
        }

        fn read_file(&self, path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
            let bytes = self
                .files
                .get(path)
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?;
            let keep = usize::try_from(limit.saturating_add(1)).unwrap_or(usize::MAX);
            Ok(bytes.iter().take(keep).copied().collect())
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn the_process_environment_reads_at_most_one_byte_past_the_limit() {
        let path = std::env::temp_dir().join(format!(
            "packdb-cli-env-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&path, b"0123456789").expect("write");
        let read = ProcessEnvironment.read_file(&path, 4).expect("read");
        std::fs::remove_file(&path).expect("remove");
        assert_eq!(read, b"01234");
        assert!(
            ProcessEnvironment
                .read_file(&path, 4)
                .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        );
    }

    #[test]
    fn the_process_environment_reports_unset_variables_as_absent() {
        assert_eq!(
            ProcessEnvironment.var("PACKDB_CLI_TEST_VARIABLE_THAT_IS_NEVER_SET"),
            None
        );
    }
}
