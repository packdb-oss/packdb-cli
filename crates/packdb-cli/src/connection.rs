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

//! Where a command connects and how it authenticates.

use std::path::{Path, PathBuf};
use std::time::Duration;

use packdb_client::AuthConfig;

use crate::environment::Environment;

/// The endpoint used when neither `--endpoint` nor `PACKDB_ENDPOINT` is set.
pub const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:4000";

const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_CREDENTIAL_BYTES: u64 = 64 * 1024;

/// Where to connect and how to authenticate.
///
/// Secrets are never accepted as argument values, which leak through process
/// listings and shell history: credential flags name a file, and environment
/// variables carry the value or a `_FILE` path.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct ConnectionArgs {
    /// Server URL [env: PACKDB_ENDPOINT] [default: http://127.0.0.1:4000]
    #[arg(long, value_name = "URL")]
    pub endpoint: Option<String>,

    /// Flight SQL endpoint for `query sql` [env: PACKDB_FLIGHT_ENDPOINT]
    #[arg(long, value_name = "URL")]
    pub flight_endpoint: Option<String>,

    /// File holding an API key, sent as X-API-Key [env: PACKDB_API_KEY or PACKDB_API_KEY_FILE]
    #[arg(long, value_name = "PATH")]
    pub api_key_file: Option<PathBuf>,

    /// File holding a bearer token [env: PACKDB_TOKEN or PACKDB_TOKEN_FILE]
    #[arg(long, value_name = "PATH")]
    pub token_file: Option<PathBuf>,

    /// File holding user:password for basic authentication [env: PACKDB_BASIC_AUTH or PACKDB_BASIC_AUTH_FILE]
    #[arg(long, value_name = "PATH")]
    pub basic_auth_file: Option<PathBuf>,

    /// Tenant, sent as X-Scope-OrgID only when given [env: PACKDB_TENANT]
    #[arg(long, value_name = "ID")]
    pub tenant: Option<String>,

    /// Request timeout in seconds [env: PACKDB_TIMEOUT] [default: 30]
    #[arg(long, value_name = "SECS")]
    pub timeout: Option<u64>,
}

/// A connection resolved from flags and the environment.
pub(crate) struct Connection {
    pub(crate) endpoint: String,
    pub(crate) flight_endpoint: Option<String>,
    pub(crate) auth: AuthConfig,
    pub(crate) tenant: Option<String>,
    pub(crate) timeout: Duration,
}

/// Invalid usage or local configuration. Never contains a secret.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct UsageError(pub(crate) String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    ApiKey,
    Token,
    BasicAuth,
}

impl Kind {
    const ALL: [Kind; 3] = [Kind::ApiKey, Kind::Token, Kind::BasicAuth];

    fn flag(self) -> &'static str {
        match self {
            Kind::ApiKey => "--api-key-file",
            Kind::Token => "--token-file",
            Kind::BasicAuth => "--basic-auth-file",
        }
    }

    fn var(self) -> &'static str {
        match self {
            Kind::ApiKey => "PACKDB_API_KEY",
            Kind::Token => "PACKDB_TOKEN",
            Kind::BasicAuth => "PACKDB_BASIC_AUTH",
        }
    }

    fn file_var(self) -> &'static str {
        match self {
            Kind::ApiKey => "PACKDB_API_KEY_FILE",
            Kind::Token => "PACKDB_TOKEN_FILE",
            Kind::BasicAuth => "PACKDB_BASIC_AUTH_FILE",
        }
    }

    fn auth(self, secret: String, source: &str) -> Result<AuthConfig, UsageError> {
        match self {
            Kind::ApiKey => Ok(AuthConfig::api_key(secret)),
            Kind::Token => Ok(AuthConfig::bearer_token(secret)),
            Kind::BasicAuth => AuthConfig::parse_basic_auth(&secret)
                .map_err(|_| UsageError(format!("{source} must hold user:password"))),
        }
    }
}

/// Where a credential came from, and how to read it.
enum Secret {
    /// A file named by a flag or a `_FILE` variable.
    File { source: String, path: PathBuf },
    /// The value of an environment variable.
    Value { source: String, value: String },
}

impl Secret {
    fn source(&self) -> &str {
        match self {
            Secret::File { source, .. } | Secret::Value { source, .. } => source,
        }
    }

    fn read(self, env: &dyn Environment) -> Result<String, UsageError> {
        let (source, text) = match self {
            Secret::Value { source, value } => (source, value),
            Secret::File { source, path } => {
                let text = read_credential_file(env, &path, &source)?;
                (source, text)
            }
        };
        let trimmed = text.trim_end();
        if trimmed.is_empty() {
            return Err(UsageError(format!("{source} is empty")));
        }
        Ok(trimmed.to_string())
    }
}

fn read_credential_file(
    env: &dyn Environment,
    path: &Path,
    source: &str,
) -> Result<String, UsageError> {
    let bytes = env.read_file(path, MAX_CREDENTIAL_BYTES).map_err(|error| {
        UsageError(format!("{source}: cannot read {}: {error}", path.display()))
    })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_CREDENTIAL_BYTES {
        return Err(UsageError(format!(
            "{source}: {} is larger than {} KiB",
            path.display(),
            MAX_CREDENTIAL_BYTES / 1024
        )));
    }
    String::from_utf8(bytes)
        .map_err(|_| UsageError(format!("{source}: {} is not valid UTF-8", path.display())))
}

impl ConnectionArgs {
    /// Resolve flags and the environment. Flags win over the environment.
    pub(crate) fn resolve(&self, env: &dyn Environment) -> Result<Connection, UsageError> {
        let endpoint = self
            .endpoint
            .clone()
            .or_else(|| env.var("PACKDB_ENDPOINT"))
            .unwrap_or_else(|| DEFAULT_ENDPOINT.to_string());
        let flight_endpoint = self
            .flight_endpoint
            .clone()
            .or_else(|| env.var("PACKDB_FLIGHT_ENDPOINT"));
        let tenant = self.tenant.clone().or_else(|| env.var("PACKDB_TENANT"));
        let seconds = match (self.timeout, env.var("PACKDB_TIMEOUT")) {
            (Some(seconds), _) => seconds,
            (None, Some(text)) => text.trim().parse().map_err(|_| {
                UsageError(format!(
                    "PACKDB_TIMEOUT must be a whole number of seconds, got {text:?}"
                ))
            })?,
            (None, None) => DEFAULT_TIMEOUT_SECS,
        };
        if seconds == 0 {
            return Err(UsageError("the timeout must be at least one second".into()));
        }
        Ok(Connection {
            endpoint,
            flight_endpoint,
            auth: self.credential(env)?,
            tenant,
            timeout: Duration::from_secs(seconds),
        })
    }

    /// The credential, if any. The highest level that supplies one is used in
    /// full; two at the deciding level are refused rather than one being chosen.
    fn credential(&self, env: &dyn Environment) -> Result<AuthConfig, UsageError> {
        let flags: Vec<(Kind, Secret)> = [
            (Kind::ApiKey, &self.api_key_file),
            (Kind::Token, &self.token_file),
            (Kind::BasicAuth, &self.basic_auth_file),
        ]
        .into_iter()
        .filter_map(|(kind, path)| {
            path.as_ref().map(|path| {
                (
                    kind,
                    Secret::File {
                        source: kind.flag().to_string(),
                        path: path.clone(),
                    },
                )
            })
        })
        .collect();
        if !flags.is_empty() {
            return chosen(flags, env);
        }

        let mut found = Vec::new();
        for kind in Kind::ALL {
            match (env.var(kind.var()), env.var(kind.file_var())) {
                (Some(_), Some(_)) => {
                    return Err(UsageError(format!(
                        "both {} and {} are set; set one",
                        kind.var(),
                        kind.file_var()
                    )));
                }
                (Some(value), None) => found.push((
                    kind,
                    Secret::Value {
                        source: kind.var().to_string(),
                        value,
                    },
                )),
                (None, Some(path)) => found.push((
                    kind,
                    Secret::File {
                        source: kind.file_var().to_string(),
                        path: PathBuf::from(path),
                    },
                )),
                (None, None) => {}
            }
        }
        if found.is_empty() {
            return Ok(AuthConfig::none());
        }
        chosen(found, env)
    }
}

/// The one credential at the deciding level, or a conflict naming every source.
fn chosen(mut found: Vec<(Kind, Secret)>, env: &dyn Environment) -> Result<AuthConfig, UsageError> {
    if found.len() > 1 {
        let sources: Vec<&str> = found.iter().map(|(_, secret)| secret.source()).collect();
        return Err(UsageError(format!(
            "conflicting credentials: {}; supply one",
            sources.join(" and ")
        )));
    }
    match found.pop() {
        Some((kind, secret)) => {
            let source = secret.source().to_string();
            let value = secret.read(env)?;
            kind.auth(value, &source)
        }
        None => Ok(AuthConfig::none()),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::environment::fake::FakeEnvironment;
    use packdb_client::AuthMethod;

    fn method(connection: &Connection) -> AuthMethod {
        connection.auth.method.clone()
    }

    #[test]
    fn defaults_apply_when_nothing_is_set() {
        let connection = ConnectionArgs::default()
            .resolve(&FakeEnvironment::default())
            .expect("resolves");
        assert_eq!(connection.endpoint, DEFAULT_ENDPOINT);
        assert_eq!(connection.flight_endpoint, None);
        assert_eq!(connection.tenant, None);
        assert_eq!(connection.timeout, Duration::from_secs(30));
        assert!(matches!(method(&connection), AuthMethod::None));
    }

    #[test]
    fn flags_win_over_the_environment() {
        let env = FakeEnvironment::default()
            .var("PACKDB_ENDPOINT", "http://env:1")
            .var("PACKDB_FLIGHT_ENDPOINT", "http://env:2")
            .var("PACKDB_TENANT", "env-tenant")
            .var("PACKDB_TIMEOUT", "9")
            .var("PACKDB_TOKEN", "env-token")
            .file("/key", b"flag-key\n");
        let args = ConnectionArgs {
            endpoint: Some("http://flag:1".into()),
            flight_endpoint: Some("http://flag:2".into()),
            tenant: Some("flag-tenant".into()),
            timeout: Some(5),
            api_key_file: Some("/key".into()),
            ..ConnectionArgs::default()
        };
        let connection = args.resolve(&env).expect("resolves");
        assert_eq!(connection.endpoint, "http://flag:1");
        assert_eq!(connection.flight_endpoint.as_deref(), Some("http://flag:2"));
        assert_eq!(connection.tenant.as_deref(), Some("flag-tenant"));
        assert_eq!(connection.timeout, Duration::from_secs(5));
        assert!(matches!(method(&connection), AuthMethod::ApiKey(key) if key == "flag-key"));
    }

    #[test]
    fn the_environment_supplies_what_flags_do_not() {
        let env = FakeEnvironment::default()
            .var("PACKDB_ENDPOINT", "http://env:1")
            .var("PACKDB_TIMEOUT", " 12 ")
            .var("PACKDB_TOKEN", "env-token\n");
        let connection = ConnectionArgs::default().resolve(&env).expect("resolves");
        assert_eq!(connection.endpoint, "http://env:1");
        assert_eq!(connection.timeout, Duration::from_secs(12));
        assert!(
            matches!(method(&connection), AuthMethod::BearerToken(token) if token == "env-token")
        );
    }

    #[test]
    fn each_credential_kind_reads_from_its_file_variable() {
        let env = FakeEnvironment::default()
            .var("PACKDB_BASIC_AUTH_FILE", "/basic")
            .file("/basic", b"user:pass\n");
        let connection = ConnectionArgs::default().resolve(&env).expect("resolves");
        assert!(matches!(
            method(&connection),
            AuthMethod::BasicAuth { username, password } if username == "user" && password == "pass"
        ));
    }

    fn refused(args: ConnectionArgs, env: FakeEnvironment) -> String {
        match args.resolve(&env) {
            Ok(_) => panic!("expected a usage error"),
            Err(UsageError(message)) => message,
        }
    }

    #[test]
    fn two_credential_flags_are_refused_naming_both() {
        let args = ConnectionArgs {
            api_key_file: Some("/a".into()),
            token_file: Some("/b".into()),
            ..ConnectionArgs::default()
        };
        let message = refused(args, FakeEnvironment::default());
        assert!(message.contains("--api-key-file") && message.contains("--token-file"));
    }

    #[test]
    fn two_credential_kinds_in_the_environment_are_refused() {
        let env = FakeEnvironment::default()
            .var("PACKDB_API_KEY", "secret-one")
            .var("PACKDB_TOKEN", "secret-two");
        let message = refused(ConnectionArgs::default(), env);
        assert!(message.contains("PACKDB_API_KEY") && message.contains("PACKDB_TOKEN"));
        assert!(!message.contains("secret-"), "no secret in {message}");
    }

    #[test]
    fn a_value_and_its_file_variable_together_are_refused() {
        let env = FakeEnvironment::default()
            .var("PACKDB_TOKEN", "secret")
            .var("PACKDB_TOKEN_FILE", "/t");
        let message = refused(ConnectionArgs::default(), env);
        assert!(message.contains("PACKDB_TOKEN") && message.contains("PACKDB_TOKEN_FILE"));
    }

    #[test]
    fn a_flag_credential_ignores_environment_credentials_entirely() {
        let env = FakeEnvironment::default()
            .var("PACKDB_API_KEY", "a")
            .var("PACKDB_TOKEN", "b")
            .file("/t", b"flag-token");
        let args = ConnectionArgs {
            token_file: Some("/t".into()),
            ..ConnectionArgs::default()
        };
        let connection = args.resolve(&env).expect("the flag decides");
        assert!(
            matches!(method(&connection), AuthMethod::BearerToken(token) if token == "flag-token")
        );
    }

    #[test]
    fn unusable_credential_files_are_refused_without_their_contents() {
        let too_large = vec![b'x'; 64 * 1024 + 1];
        let cases: [(&str, &[u8], &str); 4] = [
            ("/missing", b"", "cannot read"),
            ("/empty", b" \n", "is empty"),
            ("/large", &too_large, "larger than 64 KiB"),
            ("/binary", &[0xff, 0xfe], "not valid UTF-8"),
        ];
        for (path, contents, expected) in cases {
            let mut env = FakeEnvironment::default();
            if path != "/missing" {
                env = env.file(path, contents);
            }
            let args = ConnectionArgs {
                api_key_file: Some(path.into()),
                ..ConnectionArgs::default()
            };
            let message = refused(args, env);
            assert!(message.contains(expected), "{path}: {message}");
            assert!(message.contains("--api-key-file"), "{path}: {message}");
        }
    }

    #[test]
    fn a_malformed_basic_auth_value_is_refused_without_echoing_it() {
        let env = FakeEnvironment::default().var("PACKDB_BASIC_AUTH", "no-colon-secret");
        let message = refused(ConnectionArgs::default(), env);
        assert!(message.contains("user:password"));
        assert!(!message.contains("no-colon-secret"));
    }

    #[test]
    fn an_invalid_or_zero_timeout_is_refused() {
        let env = FakeEnvironment::default().var("PACKDB_TIMEOUT", "soon");
        assert!(refused(ConnectionArgs::default(), env).contains("PACKDB_TIMEOUT"));
        let args = ConnectionArgs {
            timeout: Some(0),
            ..ConnectionArgs::default()
        };
        assert!(refused(args, FakeEnvironment::default()).contains("at least one second"));
    }
}
