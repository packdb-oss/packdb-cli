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
//! Only `GET` is offered. A read throttled with `429` is sent again a bounded
//! number of times, waiting for `Retry-After` up to a cap, and never past the
//! request timeout measured from the first attempt.

use std::sync::Arc;
use std::time::{Duration, Instant};

use http::{HeaderMap, HeaderValue, StatusCode};
use packdb_client::credentials::AuthProvider;
use serde_json::Value;

use crate::connection::{Connection, UsageError};
use crate::exit::Exit;

/// The most of a response body a probe will hold. Enforced while streaming:
/// `Content-Length` is a claim, not a bound.
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// How many times a throttled read is sent again before it is reported.
const RETRY_ATTEMPTS: u32 = 2;

/// The wait before sending a throttled read again when the server names none.
const DEFAULT_RETRY_DELAY: Duration = Duration::from_millis(250);

/// The longest `Retry-After` honoured. The header is the server's advice; one
/// throttled read must not consume the whole run.
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
    pub(crate) headers: HeaderMap,
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
        })
    }

    /// `GET path` with encoded query parameters.
    pub(crate) async fn get(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<ProbeResponse, ProbeError> {
        let started = Instant::now();
        let mut attempts = 0;
        let response = loop {
            let response = self.send(path, query).await?;
            attempts += 1;
            if response.status() != StatusCode::TOO_MANY_REQUESTS || attempts > RETRY_ATTEMPTS {
                break response;
            }
            let delay = retry_delay(response.headers());
            if started.elapsed() + delay >= self.timeout {
                break response;
            }
            tokio::time::sleep(delay).await;
        };
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
            429 => {
                return Err(ProbeError::NoEvidence(format!(
                    "{path}: throttled (429) on each of {attempts} attempt(s)"
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
        let headers = response.headers().clone();
        let partial_causes = packdb_client::partial_causes_of(&headers);
        let body = self.bounded_body(path, response).await?;
        Ok(ProbeResponse {
            status,
            headers,
            body,
            partial_causes,
        })
    }

    async fn send(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<reqwest::Response, ProbeError> {
        let mut headers = HeaderMap::new();
        if let Some(tenant) = &self.tenant {
            headers.insert("x-scope-orgid", tenant.clone());
        }
        self.auth.apply(&mut headers).await.map_err(|error| {
            ProbeError::NoEvidence(format!("the credential could not be applied: {error}"))
        })?;
        let mut request = self
            .client
            .get(format!("{}{path}", self.base))
            .headers(headers);
        if !query.is_empty() {
            request = request.query(query);
        }
        request
            .send()
            .await
            .map_err(|error| self.send_error(path, &error))
    }

    async fn bounded_body(
        &self,
        path: &str,
        mut response: reqwest::Response,
    ) -> Result<Vec<u8>, ProbeError> {
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| self.send_error(path, &error))?
        {
            // Checked before appending: a truncated document could parse into
            // a smaller, cleaner-looking answer.
            if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(ProbeError::NoEvidence(format!(
                    "{path}: answered with more than {MAX_RESPONSE_BYTES} bytes"
                )));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    fn send_error(&self, path: &str, error: &reqwest::Error) -> ProbeError {
        if error.is_timeout() {
            ProbeError::Unreachable(format!(
                "{path}: no answer within {}s",
                self.timeout.as_secs_f64()
            ))
        } else if error.is_connect() {
            ProbeError::Unreachable(format!("{path}: cannot connect: {error}"))
        } else {
            ProbeError::NoEvidence(format!("{path}: the request failed: {error}"))
        }
    }
}

/// The wait before sending a throttled read again: `Retry-After` in seconds,
/// capped. The HTTP-date form is not honoured; it would trust a remote clock.
fn retry_delay(headers: &HeaderMap) -> Duration {
    headers
        .get(http::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map_or(DEFAULT_RETRY_DELAY, Duration::from_secs)
        .min(MAX_RETRY_DELAY)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_is_honoured_up_to_its_cap() {
        let mut headers = HeaderMap::new();
        assert_eq!(retry_delay(&headers), DEFAULT_RETRY_DELAY);
        headers.insert(http::header::RETRY_AFTER, HeaderValue::from_static("1"));
        assert_eq!(retry_delay(&headers), Duration::from_secs(1));
        headers.insert(http::header::RETRY_AFTER, HeaderValue::from_static("3600"));
        assert_eq!(retry_delay(&headers), MAX_RETRY_DELAY);
        headers.insert(
            http::header::RETRY_AFTER,
            HeaderValue::from_static("Wed, 21 Oct 2015 07:28:00 GMT"),
        );
        assert_eq!(retry_delay(&headers), DEFAULT_RETRY_DELAY);
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
