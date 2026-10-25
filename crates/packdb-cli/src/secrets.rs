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

//! A fail-closed secret scan for anything the support bundle would write.
//!
//! A scan names only the class of what it found, never the text, so a report
//! about a secret cannot leak it. It looks for credential-shaped text and for
//! the exact credential this run was configured with, whatever its length: in
//! the raw bytes, in its JSON-escaped form, and in every string of a JSON
//! document once decoded, so no escaping hides it.

use packdb_client::{AuthConfig, AuthMethod};
use regex::Regex;

/// Credential-shaped text, by class.
const PATTERNS: [(&str, &str); 7] = [
    ("private_key", r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----"),
    (
        "jwt",
        r"eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
    ),
    (
        "authorization_header",
        r#"(?i)\bauthorization\b["']?\s*[:=]"#,
    ),
    ("bearer_token", r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{8,}"),
    (
        "credential_assignment",
        r#"(?i)\b(?:password|passwd|secret|api[_-]?key|access[_-]?key|secret[_-]?key|token)\b["']?\s*[:=]\s*["']?[^\s"',{}]{4,}"#,
    ),
    (
        "url_userinfo",
        r#"(?i)\b[a-z][a-z0-9+.-]*://[^/\s:@"']+:[^/\s@"']+@"#,
    ),
    ("aws_access_key_id", r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b"),
];

pub(crate) struct SecretScanner {
    patterns: Vec<(&'static str, Regex)>,
    literals: Vec<String>,
}

impl SecretScanner {
    /// A scanner that also matches the credential `auth` holds.
    pub(crate) fn new(auth: &AuthConfig) -> Result<SecretScanner, String> {
        let patterns = PATTERNS
            .iter()
            .map(|(class, pattern)| {
                Regex::new(pattern)
                    .map(|regex| (*class, regex))
                    .map_err(|error| format!("secret pattern {class}: {error}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let literals = match &auth.method {
            AuthMethod::ApiKey(key) => vec![key.clone()],
            AuthMethod::BearerToken(token) => vec![token.clone()],
            AuthMethod::BasicAuth { username, password } => {
                vec![format!("{username}:{password}"), password.clone()]
            }
            _ => Vec::new(),
        };
        Ok(SecretScanner {
            patterns,
            literals: literals
                .into_iter()
                .filter(|literal| !literal.is_empty())
                .collect(),
        })
    }

    /// The class of the first secret in `bytes`, if any.
    pub(crate) fn scan(&self, bytes: &[u8]) -> Option<&'static str> {
        let text = String::from_utf8_lossy(bytes);
        if let Some(class) = self.scan_text(&text) {
            return Some(class);
        }
        // A JSON document is also read as the strings it decodes to, so an
        // escaped secret is found as the secret.
        let mut strings = Vec::new();
        if let Ok(document) = serde_json::from_slice::<serde_json::Value>(bytes) {
            collect_strings(&document, &mut strings);
        }
        strings.iter().find_map(|string| self.scan_text(string))
    }

    fn scan_text(&self, text: &str) -> Option<&'static str> {
        let configured = self.literals.iter().any(|literal| {
            text.contains(literal.as_str()) || {
                let escaped = serde_json::to_string(literal).unwrap_or_default();
                let escaped = escaped.trim_matches('"');
                !escaped.is_empty() && text.contains(escaped)
            }
        });
        if configured {
            return Some("configured_credential");
        }
        self.patterns
            .iter()
            .find(|(_, regex)| regex.is_match(text))
            .map(|(class, _)| *class)
    }
}

/// Every key and string value in a JSON document.
fn collect_strings(value: &serde_json::Value, strings: &mut Vec<String>) {
    match value {
        serde_json::Value::String(string) => strings.push(string.clone()),
        serde_json::Value::Array(items) => {
            for item in items {
                collect_strings(item, strings);
            }
        }
        serde_json::Value::Object(map) => {
            for (key, item) in map {
                strings.push(key.clone());
                collect_strings(item, strings);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn scanner(auth: AuthConfig) -> SecretScanner {
        SecretScanner::new(&auth).expect("patterns compile")
    }

    #[test]
    fn each_class_is_found_and_named_not_quoted() {
        let scanner = scanner(AuthConfig::none());
        let cases = [
            ("-----BEGIN RSA PRIVATE KEY-----\nMIIE", "private_key"),
            (
                // A synthetic test token, not a credential.
                "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcdefghij", // gitleaks:allow
                "jwt",
            ),
            ("Authorization: Basic dXNlcjpwYXNz", "authorization_header"),
            ("got bearer abcdefgh12345678 from", "bearer_token"),
            (r#"{"password": "hunter22"}"#, "credential_assignment"),
            ("api_key=abcd1234", "credential_assignment"),
            ("postgres://user:pass@db/x", "url_userinfo"),
            // A synthetic key ID, not a credential.
            ("key AKIAABCDEFGHIJKLMNOP here", "aws_access_key_id"), // gitleaks:allow
        ];
        for (text, class) in cases {
            assert_eq!(scanner.scan(text.as_bytes()), Some(class), "{text}");
        }
    }

    #[test]
    fn ordinary_bundle_text_is_clean() {
        let scanner = scanner(AuthConfig::none());
        for text in [
            r#"{"name": "packdb", "version": "0.2.0", "environment": "production"}"#,
            r#"{"variables_set": ["PACKDB_API_KEY", "PACKDB_TOKEN_FILE"], "credential": "api_key"}"#,
            r#"{"endpoint": "http://127.0.0.1:8080", "tenant_supplied": true}"#,
            "the route resolves the request's tenant and can create tenant state",
        ] {
            assert_eq!(scanner.scan(text.as_bytes()), None, "{text}");
        }
    }

    #[test]
    fn the_configured_credential_is_found_wherever_it_appears() {
        let key = scanner(AuthConfig::api_key("s3cr3t-value".into()));
        assert_eq!(
            key.scan(b"plain text s3cr3t-value here"),
            Some("configured_credential")
        );
        let basic = scanner(AuthConfig {
            method: AuthMethod::BasicAuth {
                username: "alice".into(),
                password: "opensesame".into(),
            },
        });
        assert_eq!(
            basic.scan(b"the password opensesame leaked"),
            Some("configured_credential")
        );
        // However short, the configured credential is found; it can withhold
        // more than it needs to, never less.
        let short = scanner(AuthConfig::api_key("k3".into()));
        assert_eq!(
            short.scan(b"{\"v\": \"xk3x\"}"),
            Some("configured_credential")
        );
    }

    #[test]
    fn an_escaped_credential_is_found_as_the_credential() {
        let quoted = scanner(AuthConfig::api_key(r#"pa"ss\word"#.into()));
        // As a JSON encoder writes it.
        let body = serde_json::to_vec(&serde_json::json!({"echo": r#"pa"ss\word"#})).expect("json");
        assert!(!String::from_utf8_lossy(&body).contains(r#"pa"ss\word"#));
        assert_eq!(quoted.scan(&body), Some("configured_credential"));
        // Every character escaped as \u: only decoding finds it.
        let unicode = scanner(AuthConfig::api_key("key-1".into()));
        let escaped = br#"{"echo": "\u006b\u0065\u0079\u002d\u0031"}"#;
        assert_eq!(unicode.scan(escaped), Some("configured_credential"));
        // A credential used as a key is found too.
        assert_eq!(
            unicode.scan(br#"{"key-1": 1}"#),
            Some("configured_credential")
        );
    }
}
