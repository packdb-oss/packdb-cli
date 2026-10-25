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

//! Bounded reads of a server's HTTP surface, for the diagnostics commands.
//!
//! A probe hands back the status, headers and body it received and leaves the
//! verdict to the check that asked: only the check knows which answer proves
//! its handler ran. What a probe does decide is when no answer was obtained at
//! all — a rejected credential, a throttle, a redirect, a timeout or an
//! oversized body — because none of those is evidence about the surface.
//!
//! Reads are `GET`; the only write is [`Prober::post_json_once`], which is
//! sent exactly once and never retried. One deadline, the request timeout measured from the
//! first attempt, covers every attempt, every wait and the body: each attempt
//! is given only the time that remains. A read throttled with `429` is sent
//! again at most twice, and only when the wait the server asks for is short
//! and fits before the deadline; otherwise the throttle is reported.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use http::{HeaderMap, HeaderValue, StatusCode};
use packdb_client::credentials::AuthProvider;
use serde_json::Value;

use crate::connection::{Connection, UsageError};
use crate::exit::Exit;

/// The most of a response body a probe will hold. Enforced while streaming:
/// `Content-Length` is a claim, not a bound.
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// The most of a write acknowledgement a probe will hold. An acknowledgement
/// is a small protobuf; nothing legitimate approaches this.
const MAX_ACK_BYTES: usize = 256 * 1024;

/// How many times a throttled read is sent again before it is reported.
const RETRY_ATTEMPTS: u32 = 2;

/// The wait before sending a throttled read again when the server names none.
const DEFAULT_RETRY_DELAY: Duration = Duration::from_millis(250);

/// The longest `Retry-After` waited for. A longer request is not shortened —
/// that would retry before the server asked — so the throttle is reported.
const MAX_RETRY_DELAY: Duration = Duration::from_secs(2);

/// Why a probe obtained no answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProbeError {
    /// `401`: the server did not accept the credential.
    Unauthenticated(String),
    /// `403`: the credential may not read this route.
    PermissionDenied(String),
    /// The server could not be reached, or did not answer in time.
    Unreachable(String),
    /// An answer arrived that says nothing about the surface: a throttle that
    /// persisted, a redirect, a proxy demand or an oversized body.
    NoEvidence(String),
}

impl ProbeError {
    pub(crate) fn message(&self) -> &str {
        match self {
            ProbeError::Unauthenticated(message)
            | ProbeError::PermissionDenied(message)
            | ProbeError::Unreachable(message)
            | ProbeError::NoEvidence(message) => message,
        }
    }

    /// The command status this failure implies on its own.
    pub(crate) fn exit(&self) -> Exit {
        match self {
            ProbeError::Unauthenticated(_) => Exit::Unauthenticated,
            ProbeError::PermissionDenied(_) => Exit::PermissionDenied,
            ProbeError::Unreachable(_) => Exit::Unreachable,
            ProbeError::NoEvidence(_) => Exit::Incomplete,
        }
    }
}

/// An answer, unclassified.
#[derive(Debug)]
pub(crate) struct ProbeResponse {
    pub(crate) status: StatusCode,
    pub(crate) content_type: Option<String>,
    pub(crate) body: Vec<u8>,
    pub(crate) partial_causes: Vec<String>,
}

impl ProbeResponse {
    pub(crate) fn json(&self) -> Option<Value> {
        serde_json::from_slice(&self.body).ok()
    }
}

/// Reads one server's HTTP surface with the resolved connection's credential,
/// tenant and timeout.
pub(crate) struct Prober {
    client: reqwest::Client,
    base: String,
    tenant: Option<HeaderValue>,
    auth: Arc<dyn AuthProvider>,
    timeout: Duration,
    /// Whether any response arrived, whatever its status or usability.
    responded: AtomicBool,
}

impl Prober {
    pub(crate) fn new(connection: Connection) -> Result<Prober, UsageError> {
        let tenant = match &connection.tenant {
            Some(tenant) => Some(
                HeaderValue::from_str(tenant)
                    .map_err(|_| UsageError("the tenant is not a valid header value".into()))?,
            ),
            None => None,
        };
        let client = reqwest::Client::builder()
            // A redirect is never followed: the answer at its destination is
            // not evidence about this server, and the credential must not
            // follow it to a host nobody chose.
            .redirect(reqwest::redirect::Policy::none())
            .timeout(connection.timeout)
            .build()
            .map_err(|error| UsageError(format!("cannot build the HTTP client: {error}")))?;
        Ok(Prober {
            client,
            base: connection.endpoint.trim_end_matches('/').to_string(),
            tenant,
            auth: connection.auth.into_provider(),
            timeout: connection.timeout,
            responded: AtomicBool::new(false),
        })
    }

    /// Whether the server sent any response: a `401`, a throttle or a redirect
    /// counts, because the server was reached even though it gave no evidence.
    pub(crate) fn responded(&self) -> bool {
        self.responded.load(Ordering::Relaxed)
    }

    /// `GET path`, within the request timeout.
    pub(crate) async fn get(&self, path: &str) -> Result<ProbeResponse, ProbeError> {
        let deadline = tokio::time::Instant::now() + self.timeout;
        match tokio::time::timeout_at(deadline, self.get_until(path, deadline)).await {
            Ok(result) => result,
            Err(_) => Err(self.timed_out(path)),
        }
    }

    async fn get_until(
        &self,
        path: &str,
        deadline: tokio::time::Instant,
    ) -> Result<ProbeResponse, ProbeError> {
        let mut attempts = 0;
        let response = loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(self.timed_out(path));
            }
            let response = self
                .send(reqwest::Method::GET, path, None, remaining)
                .await?;
            attempts += 1;
            if response.status() != StatusCode::TOO_MANY_REQUESTS {
                break response;
            }
            let wait = match retry_delay(response.headers()) {
                Ok(wait) if attempts <= RETRY_ATTEMPTS => wait,
                Ok(_) => {
                    return Err(ProbeError::NoEvidence(format!(
                        "{path}: throttled (429) on each of {attempts} attempt(s)"
                    )));
                }
                Err(asked) => {
                    return Err(ProbeError::NoEvidence(format!(
                        "{path}: throttled (429); the server asked to wait {asked}, \
                         longer than packdb waits"
                    )));
                }
            };
            if tokio::time::Instant::now() + wait >= deadline {
                return Err(ProbeError::NoEvidence(format!(
                    "{path}: throttled (429); waiting {}s would pass the {}s timeout",
                    wait.as_secs_f64(),
                    self.timeout.as_secs_f64()
                )));
            }
            tokio::time::sleep(wait).await;
        };
        self.answer(path, response, MAX_RESPONSE_BYTES).await
    }

    /// `POST path` with a JSON body, sent exactly once within the request
    /// timeout. A write is never retried, not even after a `429`.
    pub(crate) async fn post_json_once(
        &self,
        path: &str,
        body: Vec<u8>,
    ) -> Result<ProbeResponse, ProbeError> {
        let write = async {
            let response = self
                .send(reqwest::Method::POST, path, Some(body), self.timeout)
                .await?;
            if response.status() == StatusCode::TOO_MANY_REQUESTS {
                return Err(ProbeError::NoEvidence(format!(
                    "{path}: throttled (429); a write is not retried"
                )));
            }
            self.answer(path, response, MAX_ACK_BYTES).await
        };
        match tokio::time::timeout(self.timeout, write).await {
            Ok(result) => result,
            Err(_) => Err(self.timed_out(path)),
        }
    }

    /// Classify an answer that arrived, then read its body under `max_bytes`.
    async fn answer(
        &self,
        path: &str,
        response: reqwest::Response,
        max_bytes: usize,
    ) -> Result<ProbeResponse, ProbeError> {
        let status = response.status();
        match status.as_u16() {
            401 => {
                return Err(ProbeError::Unauthenticated(format!(
                    "{path}: not authenticated (401)"
                )));
            }
            403 => {
                return Err(ProbeError::PermissionDenied(format!(
                    "{path}: permission denied (403)"
                )));
            }
            407 => {
                return Err(ProbeError::NoEvidence(format!(
                    "{path}: a proxy demanded authentication (407)"
                )));
            }
            408 => {
                return Err(ProbeError::Unreachable(format!(
                    "{path}: the server timed out before answering (408)"
                )));
            }
            _ => {}
        }
        if status.is_redirection() {
            return Err(ProbeError::NoEvidence(format!(
                "{path}: answered with a redirect ({}), which is not followed",
                status.as_u16()
            )));
        }
        let partial_causes = packdb_client::partial_causes_of(response.headers());
        let content_type = response
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let body = self.bounded_body(path, response, max_bytes).await?;
        Ok(ProbeResponse {
            status,
            content_type,
            body,
            partial_causes,
        })
    }

    fn timed_out(&self, path: &str) -> ProbeError {
        ProbeError::Unreachable(format!(
            "{path}: no answer within {}s",
            self.timeout.as_secs_f64()
        ))
    }

    /// One attempt, allowed `remaining` from connecting to the end of its body.
    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Vec<u8>>,
        remaining: Duration,
    ) -> Result<reqwest::Response, ProbeError> {
        let mut headers = HeaderMap::new();
        if let Some(tenant) = &self.tenant {
            headers.insert("x-scope-orgid", tenant.clone());
        }
        self.auth.apply(&mut headers).await.map_err(|error| {
            ProbeError::NoEvidence(format!("the credential could not be applied: {error}"))
        })?;
        if body.is_some() {
            headers.insert(
                http::header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
        }
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.base))
            .timeout(remaining)
            .headers(headers);
        if let Some(body) = body {
            request = request.body(body);
        }
        request
            .send()
            .await
            .inspect(|_| self.responded.store(true, Ordering::Relaxed))
            .map_err(|error| self.send_error(path, &error))
    }

    async fn bounded_body(
        &self,
        path: &str,
        mut response: reqwest::Response,
        max_bytes: usize,
    ) -> Result<Vec<u8>, ProbeError> {
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| self.send_error(path, &error))?
        {
            // Checked before appending: a truncated document could parse into
            // a smaller, cleaner-looking answer.
            if body.len().saturating_add(chunk.len()) > max_bytes {
                return Err(ProbeError::NoEvidence(format!(
                    "{path}: answered with more than {max_bytes} bytes"
                )));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    fn send_error(&self, path: &str, error: &reqwest::Error) -> ProbeError {
        if error.is_timeout() {
            self.timed_out(path)
        } else if error.is_connect() {
            ProbeError::Unreachable(format!("{path}: cannot connect: {error}"))
        } else {
            ProbeError::NoEvidence(format!("{path}: the request failed: {error}"))
        }
    }
}

/// The wait before sending a throttled read again: `Retry-After` in seconds,
/// or the default when the server names none. `Err` holds the server's request
/// when doctor will not honour it: longer than the cap, or an HTTP date, which
/// would mean trusting a remote clock.
fn retry_delay(headers: &HeaderMap) -> Result<Duration, String> {
    let Some(value) = headers.get(http::header::RETRY_AFTER) else {
        return Ok(DEFAULT_RETRY_DELAY);
    };
    let text = value.to_str().unwrap_or("").trim();
    match text.parse::<u64>() {
        Ok(seconds) if Duration::from_secs(seconds) <= MAX_RETRY_DELAY => {
            Ok(Duration::from_secs(seconds))
        }
        Ok(seconds) => Err(format!("{seconds}s")),
        Err(_) => Err(format!("until {text:?}")),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn a_retry_after_longer_than_the_cap_is_not_shortened() {
        let mut headers = HeaderMap::new();
        assert_eq!(retry_delay(&headers), Ok(DEFAULT_RETRY_DELAY));
        headers.insert(http::header::RETRY_AFTER, HeaderValue::from_static("2"));
        assert_eq!(retry_delay(&headers), Ok(Duration::from_secs(2)));
        headers.insert(http::header::RETRY_AFTER, HeaderValue::from_static("3600"));
        assert_eq!(retry_delay(&headers), Err("3600s".to_string()));
        headers.insert(
            http::header::RETRY_AFTER,
            HeaderValue::from_static("Wed, 21 Oct 2015 07:28:00 GMT"),
        );
        assert!(retry_delay(&headers).is_err());
    }

    fn prober(endpoint: String, timeout: Duration) -> Prober {
        Prober::new(Connection {
            endpoint,
            flight_endpoint: None,
            auth: packdb_client::AuthConfig::none(),
            tenant: None,
            timeout,
        })
        .expect("prober")
    }

    #[tokio::test]
    async fn a_long_retry_after_is_reported_without_retrying() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "3600"))
            .mount(&server)
            .await;
        let prober = prober(server.uri(), Duration::from_secs(5));
        let started = std::time::Instant::now();
        let error = prober.get("/ready").await.expect_err("throttled");
        let elapsed = started.elapsed();
        assert!(
            matches!(&error, ProbeError::NoEvidence(m) if m.contains("asked to wait 3600s")),
            "{error:?}"
        );
        assert_eq!(server.received_requests().await.expect("recorded").len(), 1);
        // Only a guard against a hang: answering before the timeout shows
        // nothing was waited out.
        assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
    }

    #[tokio::test]
    async fn one_deadline_covers_every_attempt_and_wait() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "1"))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
            .mount(&server)
            .await;
        let prober = prober(server.uri(), Duration::from_millis(1800));
        let started = std::time::Instant::now();
        let error = prober.get("/ready").await.expect_err("deadline");
        let elapsed = started.elapsed();
        // The second attempt gets what remains after the 1 s wait, not a fresh
        // 1.8 s.
        assert!(elapsed >= Duration::from_secs(1), "{elapsed:?}");
        assert!(elapsed < Duration::from_millis(2300), "{elapsed:?}");
        assert!(
            matches!(&error, ProbeError::Unreachable(m) if m.contains("no answer within 1.8s")),
            "{error:?}"
        );
        assert_eq!(server.received_requests().await.expect("recorded").len(), 2);
    }

    #[tokio::test]
    async fn a_wait_that_would_pass_the_deadline_is_not_taken() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "2"))
            .mount(&server)
            .await;
        // Building the client is setup, not the request; only the request is
        // timed.
        let prober = prober(server.uri(), Duration::from_secs(1));
        let started = std::time::Instant::now();
        let error = prober.get("/ready").await.expect_err("throttled");
        let elapsed = started.elapsed();
        // Reported as a throttle, not as the timeout a wait would have run
        // into, and sent once: the wait was neither taken nor shortened.
        assert!(
            matches!(&error, ProbeError::NoEvidence(m) if m.contains("would pass the 1s timeout")),
            "{error:?}"
        );
        assert_eq!(server.received_requests().await.expect("recorded").len(), 1);
        // Only a guard against a hang: well short of the 2 s the server asked
        // for.
        assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
    }

    #[test]
    fn each_failure_implies_its_exit() {
        let cases = [
            (
                ProbeError::Unauthenticated("a".into()),
                Exit::Unauthenticated,
            ),
            (
                ProbeError::PermissionDenied("b".into()),
                Exit::PermissionDenied,
            ),
            (ProbeError::Unreachable("c".into()), Exit::Unreachable),
            (ProbeError::NoEvidence("d".into()), Exit::Incomplete),
        ];
        for (error, exit) in cases {
            assert_eq!(error.exit(), exit, "{error:?}");
            assert!(!error.message().is_empty());
        }
    }
}
