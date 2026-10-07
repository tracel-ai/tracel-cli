use std::fmt::{self, Display};
use std::time::SystemTime;

use chrono::{DateTime, Utc};
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

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

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
