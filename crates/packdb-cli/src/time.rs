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

//! Times and durations accepted by query commands, and their wire forms.
//!
//! Every command reads times the same way: RFC 3339 in UTC
//! (`2026-10-23T12:00:00Z`), Unix seconds (`1761220800` or `1761220800.5`),
//! `now`, or `now-<duration>` (`now-15m`). Each API then receives the form it
//! expects, without losing precision.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::connection::UsageError;

const NANOS_PER_SECOND: i128 = 1_000_000_000;

/// An instant, in nanoseconds since the Unix epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Timestamp(i128);

impl Timestamp {
    pub(crate) fn now() -> Timestamp {
        Timestamp::from_system(SystemTime::now())
    }

    fn from_system(time: SystemTime) -> Timestamp {
        match time.duration_since(UNIX_EPOCH) {
            Ok(after) => Timestamp(i128::try_from(after.as_nanos()).unwrap_or(i128::MAX)),
            Err(before) => {
                Timestamp(-i128::try_from(before.duration().as_nanos()).unwrap_or(i128::MAX))
            }
        }
    }

    fn minus(self, duration: Duration) -> Timestamp {
        Timestamp(self.0 - i128::try_from(duration.as_nanos()).unwrap_or(i128::MAX))
    }

    /// Unix seconds with the fraction the instant has, for Prometheus.
    pub(crate) fn seconds(self) -> String {
        if self.0 < 0 {
            return format!("-{}", Timestamp(-self.0).seconds());
        }
        let whole = self.0.div_euclid(NANOS_PER_SECOND);
        let nanos = self.0.rem_euclid(NANOS_PER_SECOND);
        if nanos == 0 {
            whole.to_string()
        } else {
            let fraction = format!("{nanos:09}");
            format!("{whole}.{}", fraction.trim_end_matches('0'))
        }
    }

    /// Unix nanoseconds, for Loki.
    pub(crate) fn nanos(self) -> String {
        self.0.to_string()
    }

    /// Whole Unix seconds rounded down, for a Tempo window start.
    pub(crate) fn floor_seconds(self, flag: &str) -> Result<u64, UsageError> {
        u64::try_from(self.0.div_euclid(NANOS_PER_SECOND))
            .map_err(|_| UsageError(format!("{flag} must not be before 1970")))
    }

    /// Whole Unix seconds rounded up, for a Tempo window end.
    pub(crate) fn ceil_seconds(self, flag: &str) -> Result<u64, UsageError> {
        let whole = self.0.div_euclid(NANOS_PER_SECOND);
        let up = if self.0.rem_euclid(NANOS_PER_SECOND) == 0 {
            whole
        } else {
            whole + 1
        };
        u64::try_from(up).map_err(|_| UsageError(format!("{flag} must not be before 1970")))
    }
}

fn time_error(flag: &str, text: &str) -> UsageError {
    UsageError(format!(
        "{flag} must be RFC 3339 in UTC, Unix seconds, `now` or `now-<duration>`, got {text:?}"
    ))
}

/// Parse a time argument against `now`.
pub(crate) fn parse_time(text: &str, flag: &str, now: Timestamp) -> Result<Timestamp, UsageError> {
    let text = text.trim();
    if text == "now" {
        return Ok(now);
    }
    if let Some(ago) = text.strip_prefix("now-") {
        let duration = humantime::parse_duration(ago).map_err(|_| time_error(flag, text))?;
        return Ok(now.minus(duration));
    }
    if let Some(seconds) = parse_unix_seconds(text) {
        return Ok(seconds);
    }
    humantime::parse_rfc3339(text)
        .map(Timestamp::from_system)
        .map_err(|_| time_error(flag, text))
}

/// `123` or `123.456`, to the nanosecond, without floating-point rounding.
fn parse_unix_seconds(text: &str) -> Option<Timestamp> {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || fraction.len() > 9
        || !fraction.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let whole: i128 = whole.parse().ok()?;
    let nanos: i128 = if fraction.is_empty() {
        0
    } else {
        format!("{fraction:0<9}").parse().ok()?
    };
    whole
        .checked_mul(NANOS_PER_SECOND)?
        .checked_add(nanos)
        .map(Timestamp)
}

/// Parse a positive duration such as `15s`, `1m` or `1h30m`.
pub(crate) fn parse_duration(text: &str, flag: &str) -> Result<Duration, UsageError> {
    let duration = humantime::parse_duration(text.trim()).map_err(|_| {
        UsageError(format!(
            "{flag} must be a duration such as 15s or 1h, got {text:?}"
        ))
    })?;
    if duration.as_millis() == 0 {
        return Err(UsageError(format!("{flag} must be at least 1ms")));
    }
    Ok(duration)
}

/// A duration in whole milliseconds, a form Prometheus and Loki both accept.
pub(crate) fn duration_text(duration: Duration) -> String {
    format!("{}ms", duration.as_millis())
}

/// A query window, defaulting to the hour before now.
pub(crate) struct Window {
    pub(crate) start: Timestamp,
    pub(crate) end: Timestamp,
}

impl Window {
    pub(crate) fn resolve(
        start: Option<&str>,
        end: Option<&str>,
        now: Timestamp,
    ) -> Result<Window, UsageError> {
        let end = match end {
            Some(text) => parse_time(text, "--end", now)?,
            None => now,
        };
        let start = match start {
            Some(text) => parse_time(text, "--start", now)?,
            None => end.minus(Duration::from_secs(3600)),
        };
        if start > end {
            return Err(UsageError("--start must not be after --end".into()));
        }
        Ok(Window { start, end })
    }

    /// The default step for a range query: the window over 250 points, at
    /// least one second.
    pub(crate) fn default_step(&self) -> Duration {
        let span = u64::try_from((self.end.0 - self.start.0) / 250).unwrap_or(0);
        Duration::from_nanos(span).max(Duration::from_secs(1))
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    const NOW: Timestamp = Timestamp(1_700_000_000 * NANOS_PER_SECOND);

    fn at(text: &str) -> Timestamp {
        parse_time(text, "--time", NOW).expect("parses")
    }

    #[test]
    fn every_time_form_parses_to_the_nanosecond() {
        assert_eq!(at("now"), NOW);
        assert_eq!(at("now-90s"), Timestamp(NOW.0 - 90 * NANOS_PER_SECOND));
        assert_eq!(at("1700000000"), NOW);
        assert_eq!(at("1700000000.000000001"), Timestamp(NOW.0 + 1));
        assert_eq!(at("1700000000.5"), Timestamp(NOW.0 + 500_000_000));
        assert_eq!(at("2023-11-14T22:13:20Z"), NOW);
        assert_eq!(at(" 1700000000 "), NOW);
    }

    #[test]
    fn malformed_times_are_refused_naming_the_flag() {
        for text in [
            "",
            "yesterday",
            "1.2.3",
            "-5",
            "1700000000.1234567890",
            "now-soon",
            "2023-11-14",
        ] {
            match parse_time(text, "--start", NOW) {
                Ok(time) => panic!("{text:?} parsed as {time:?}"),
                Err(UsageError(message)) => assert!(message.contains("--start"), "{message}"),
            }
        }
    }

    #[test]
    fn wire_forms_keep_precision() {
        let t = Timestamp(1_700_000_000 * NANOS_PER_SECOND + 250_000_000);
        assert_eq!(t.seconds(), "1700000000.25");
        assert_eq!(NOW.seconds(), "1700000000");
        assert_eq!(t.nanos(), "1700000000250000000");
        assert_eq!(t.floor_seconds("--start").expect("floor"), 1_700_000_000);
        assert_eq!(t.ceil_seconds("--end").expect("ceil"), 1_700_000_001);
        assert_eq!(NOW.ceil_seconds("--end").expect("exact"), 1_700_000_000);
        assert!(Timestamp(-1).floor_seconds("--start").is_err());
        assert!(
            Timestamp(-NANOS_PER_SECOND - 1)
                .ceil_seconds("--end")
                .is_err()
        );
        assert_eq!(Timestamp(-1).seconds(), "-0.000000001");
        assert_eq!(Timestamp(-NANOS_PER_SECOND).seconds(), "-1");
    }

    #[test]
    fn durations_parse_and_must_be_positive() {
        assert_eq!(
            parse_duration("15s", "--step").expect("15s"),
            Duration::from_secs(15)
        );
        assert_eq!(
            parse_duration("1h30m", "--step").expect("1h30m"),
            Duration::from_secs(5400)
        );
        assert!(parse_duration("0s", "--step").is_err());
        assert!(parse_duration("fast", "--step").is_err());
        assert_eq!(duration_text(Duration::from_millis(1500)), "1500ms");
    }

    #[test]
    fn windows_default_to_the_last_hour_and_refuse_inversion() {
        let window = Window::resolve(None, None, NOW).expect("default");
        assert_eq!(window.end, NOW);
        assert_eq!(window.start, Timestamp(NOW.0 - 3600 * NANOS_PER_SECOND));
        assert_eq!(window.default_step(), Duration::from_nanos(14_400_000_000));
        let short = Window::resolve(Some("now-10s"), None, NOW).expect("short");
        assert_eq!(short.default_step(), Duration::from_secs(1));
        assert!(Window::resolve(Some("now"), Some("now-1m"), NOW).is_err());
        assert!(Window::resolve(Some("bad"), None, NOW).is_err());
        assert!(Window::resolve(None, Some("bad"), NOW).is_err());
    }
}
