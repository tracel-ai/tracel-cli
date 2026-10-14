use std::fmt::{self, Display};
use std::time::SystemTime;

use chrono::{DateTime, Local, TimeZone, Utc};
use serde::{Serialize, Serializer};

/// A point in time, written in RFC 3339 in UTC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(pub SystemTime);

impl Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&DateTime::<Utc>::from(self.0).to_rfc3339())
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

/// Show a server's RFC 3339 time in the local zone, to the minute. Other text stays as
/// it is.
pub fn when(text: &str) -> String {
    local(text, &Local, TO_THE_MINUTE)
}

/// Show a log entry's RFC 3339 time in the local zone, to the second.
pub fn log_time(text: &str) -> String {
    local(text, &Local, TO_THE_SECOND)
}

/// Say how long it was from the RFC 3339 time `start` to `end`, or to now without one,
/// such as "3 min 20 s". None when a time doesn't parse.
pub fn elapsed(start: &str, end: Option<&str>) -> Option<String> {
    let start = DateTime::parse_from_rfc3339(start).ok()?;
    let end = match end {
        Some(end) => DateTime::parse_from_rfc3339(end).ok()?.to_utc(),
        None => Utc::now(),
    };
    Some(span((end - start.to_utc()).num_seconds().max(0)))
}

fn span(seconds: i64) -> String {
    match seconds {
        seconds if seconds < 60 => format!("{seconds} s"),
        seconds if seconds < 3600 => format!("{} min {} s", seconds / 60, seconds % 60),
        seconds => format!("{} h {} min", seconds / 3600, seconds % 3600 / 60),
    }
}

const TO_THE_MINUTE: &str = "%Y-%m-%d %H:%M";
const TO_THE_SECOND: &str = "%Y-%m-%d %H:%M:%S";

fn local<Tz: TimeZone>(text: &str, zone: &Tz, format: &str) -> String
where
    Tz::Offset: Display,
{
    match DateTime::parse_from_rfc3339(text) {
        Ok(time) => time.with_timezone(zone).format(format).to_string(),
        Err(_) => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    #[test]
    fn given_a_server_time_when_showing_it_then_it_reads_in_the_local_zone_to_the_minute() {
        let four_hours_west = chrono::FixedOffset::west_opt(4 * 3600).unwrap();

        let time = local("2026-10-09T20:21:06.075Z", &four_hours_west, TO_THE_MINUTE);

        assert_eq!(time, "2026-10-09 16:21");
    }

    #[test]
    fn given_a_log_time_when_showing_it_then_it_keeps_its_seconds() {
        let time = local("2026-10-09T20:21:06.075Z", &Utc, TO_THE_SECOND);

        assert_eq!(time, "2026-10-09 20:21:06");
    }

    #[test]
    fn given_text_that_is_not_a_time_when_showing_it_then_it_stays_as_it_is() {
        let time = local("yesterday", &Utc, TO_THE_MINUTE);

        assert_eq!(time, "yesterday");
    }

    #[test]
    fn given_a_timestamp_when_writing_it_then_it_is_rfc_3339_in_utc() {
        let timestamp = Timestamp(UNIX_EPOCH + Duration::from_secs(1_900_000_000));

        let text = timestamp.to_string();

        assert_eq!(text, "2030-03-17T17:46:40+00:00");
    }
}
