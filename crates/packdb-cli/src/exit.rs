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

//! Exit statuses shared by every `packdb` command.

use packdb_client::ClientError;

/// The exit status of a `packdb` command. `COMPATIBILITY.md` defines each code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// 0: succeeded with a complete result.
    Success,
    /// 1: the server rejected the request, or a required check demonstrably failed.
    Rejected,
    /// 2: invalid usage or local configuration; nothing was sent.
    Usage,
    /// 3: succeeded, but completeness is partial or unknown.
    Incomplete,
    /// 4: not authenticated (401).
    Unauthenticated,
    /// 5: permission denied (403).
    PermissionDenied,
    /// 6: the server could not be reached.
    Unreachable,
    /// 7: a server or protocol failure.
    ServerFailure,
    /// 8: output could not be delivered: writing or flushing stdout or stderr
    /// failed, including warnings and error messages.
    OutputFailed,
}

impl Exit {
    /// The process exit code.
    pub fn code(self) -> u8 {
        match self {
            Exit::Success => 0,
            Exit::Rejected => 1,
            Exit::Usage => 2,
            Exit::Incomplete => 3,
            Exit::Unauthenticated => 4,
            Exit::PermissionDenied => 5,
            Exit::Unreachable => 6,
            Exit::ServerFailure => 7,
            Exit::OutputFailed => 8,
        }
    }

    /// The status that wins when both apply. Precedence, first match wins:
    /// 8, 2, 4, 5, 6, 7, 1, 3, then 0.
    pub fn worst(self, other: Exit) -> Exit {
        if self.rank() <= other.rank() {
            self
        } else {
            other
        }
    }

    fn rank(self) -> u8 {
        match self {
            Exit::OutputFailed => 0,
            Exit::Usage => 1,
            Exit::Unauthenticated => 2,
            Exit::PermissionDenied => 3,
            Exit::Unreachable => 4,
            Exit::ServerFailure => 5,
            Exit::Rejected => 6,
            Exit::Incomplete => 7,
            Exit::Success => 8,
        }
    }

    /// The status a client error reports.
    pub(crate) fn of(error: &ClientError) -> Exit {
        match error {
            ClientError::SqlError(_) => Exit::Rejected,
            ClientError::Unauthenticated(_) => Exit::Unauthenticated,
            ClientError::PermissionDenied(_) => Exit::PermissionDenied,
            ClientError::ConnectionError(_) => Exit::Unreachable,
            ClientError::ConfigError(_) | ClientError::IoError(_) => Exit::Usage,
            _ => Exit::ServerFailure,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Exit; 9] = [
        Exit::Success,
        Exit::Rejected,
        Exit::Usage,
        Exit::Incomplete,
        Exit::Unauthenticated,
        Exit::PermissionDenied,
        Exit::Unreachable,
        Exit::ServerFailure,
        Exit::OutputFailed,
    ];

    #[test]
    fn codes_are_the_documented_ones() {
        let codes: Vec<u8> = ALL.iter().map(|exit| exit.code()).collect();
        assert_eq!(codes, [0, 1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn precedence_is_8_2_4_5_6_7_1_3_0() {
        let order = [8, 2, 4, 5, 6, 7, 1, 3, 0];
        for (i, a) in ALL.iter().enumerate() {
            for b in ALL.iter().skip(i) {
                let expected = order
                    .iter()
                    .find(|code| **code == a.code() || **code == b.code())
                    .copied();
                assert_eq!(Some(a.worst(*b).code()), expected, "{a:?} vs {b:?}");
                assert_eq!(a.worst(*b), b.worst(*a), "{a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn a_demonstrated_failure_outranks_incompleteness() {
        assert_eq!(Exit::Rejected.worst(Exit::Incomplete), Exit::Rejected);
    }

    #[test]
    fn client_errors_map_to_their_status() {
        let cases = [
            (ClientError::SqlError("x".into()), Exit::Rejected),
            (
                ClientError::Unauthenticated("x".into()),
                Exit::Unauthenticated,
            ),
            (
                ClientError::PermissionDenied("x".into()),
                Exit::PermissionDenied,
            ),
            (ClientError::ConnectionError("x".into()), Exit::Unreachable),
            (ClientError::ProtocolError("x".into()), Exit::ServerFailure),
            (ClientError::ConfigError("x".into()), Exit::Usage),
            (
                ClientError::IoError(std::io::Error::other("x")),
                Exit::Usage,
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(Exit::of(&error), expected, "{error:?}");
        }
    }
}
