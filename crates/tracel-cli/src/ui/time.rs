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

/// A server's RFC 3339 time as a person reads it: local, to the minute. Text that is
/// not such a time is kept as it is.
pub fn when(text: &str) -> String {
    local(text, &Local, "%Y-%m-%d %H:%M")
}

/// A log entry's RFC 3339 time as a person reads it: local, to the second.
pub fn log_time(text: &str) -> String {
    local(text, &Local, "%Y-%m-%d %H:%M:%S")
}

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
        let zone = chrono::FixedOffset::west_opt(4 * 3600).unwrap();
        let minute = "%Y-%m-%d %H:%M";
        assert_eq!(
            local("2026-10-09T20:21:06.075Z", &zone, minute),
            "2026-10-09 16:21"
        );
        assert_eq!(
            local("2026-10-09T20:21:06Z", &Utc, minute),
            "2026-10-09 20:21"
        );
        assert_eq!(local("yesterday", &Utc, minute), "yesterday");
        assert_eq!(local("", &Utc, minute), "");
    }

    #[test]
    fn timestamps_are_rfc_3339_in_utc() {
        let time = Timestamp(UNIX_EPOCH + Duration::from_secs(1_900_000_000));
        assert_eq!(time.to_string(), "2030-03-17T17:46:40+00:00");
        assert_eq!(
            serde_json::to_value(time).unwrap(),
            "2030-03-17T17:46:40+00:00"
        );
    }
}
