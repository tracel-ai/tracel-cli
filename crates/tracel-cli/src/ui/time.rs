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
    fn server_times_read_in_the_local_zone() {
        let west = chrono::FixedOffset::west_opt(4 * 3600).unwrap();
        for (text, expected) in [
            ("2026-10-09T20:21:06.075Z", "2026-10-09 16:21"),
            ("2026-10-09T20:21:06+00:00", "2026-10-09 16:21"),
            ("yesterday", "yesterday"),
            ("", ""),
        ] {
            assert_eq!(local(text, &west, TO_THE_MINUTE), expected, "{text:?}");
        }
    }

    #[test]
    fn a_log_time_keeps_its_seconds() {
        let utc = chrono::Utc;

        let time = local("2026-10-09T20:21:06.075Z", &utc, TO_THE_SECOND);

        assert_eq!(time, "2026-10-09 20:21:06");
    }

    #[test]
    fn a_timestamp_is_rfc_3339_in_utc() {
        let time = Timestamp(UNIX_EPOCH + Duration::from_secs(1_900_000_000));

        let (text, json) = (time.to_string(), serde_json::to_value(time).unwrap());

        assert_eq!(text, "2030-03-17T17:46:40+00:00");
        assert_eq!(json, "2030-03-17T17:46:40+00:00");
    }
}
