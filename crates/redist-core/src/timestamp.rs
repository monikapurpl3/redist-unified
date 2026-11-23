//! The manifest's `issued` time.

use std::fmt;
use std::str::FromStr;

use serde::Deserialize;

/// A UTC time to the second, always written `YYYY-MM-DDTHH:MM:SSZ`.
///
/// Ordering is chronological.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(try_from = "String")]
pub struct Timestamp {
    // Field order is significant: the derived ordering compares in this order.
    year: u16,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
}

impl FromStr for Timestamp {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        let shape = || format!("timestamp {text:?} is not YYYY-MM-DDTHH:MM:SSZ");
        let b = text.as_bytes();
        if b.len() != 20
            || b[4] != b'-'
            || b[7] != b'-'
            || b[10] != b'T'
            || b[13] != b':'
            || b[16] != b':'
            || b[19] != b'Z'
        {
            return Err(shape());
        }
        let number = |from: usize, to: usize| {
            let digits = &b[from..to];
            if digits.iter().all(u8::is_ascii_digit) {
                Ok(digits
                    .iter()
                    .fold(0u16, |n, d| n * 10 + u16::from(d - b'0')))
            } else {
                Err(shape())
            }
        };
        let (year, month, day) = (number(0, 4)?, number(5, 7)?, number(8, 10)?);
        let (hour, minute, second) = (number(11, 13)?, number(14, 16)?, number(17, 19)?);
        if !(1..=12).contains(&month)
            || day == 0
            || day > days_in_month(year, month)
            || hour > 23
            || minute > 59
            || second > 59
        {
            return Err(format!("timestamp {text:?} is not a real time"));
        }
        // Every value below is at most 59 after the checks above.
        Ok(Timestamp {
            year,
            month: month as u8,
            day: day as u8,
            hour: hour as u8,
            minute: minute as u8,
            second: second as u8,
        })
    }
}

impl TryFrom<String> for Timestamp {
    type Error = String;

    fn try_from(text: String) -> Result<Self, String> {
        text.parse()
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

fn days_in_month(year: u16, month: u16) -> u16 {
    match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

#[cfg(test)]
mod tests {
    use super::Timestamp;

    fn t(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    #[test]
    fn round_trips_and_orders() {
        assert_eq!(
            t("2026-11-23T12:00:00Z").to_string(),
            "2026-11-23T12:00:00Z"
        );
        assert!(t("2026-11-23T12:00:01Z") > t("2026-11-23T12:00:00Z"));
        assert!(t("2027-01-01T00:00:00Z") > t("2026-12-31T23:59:59Z"));
        assert!(t("2026-10-02T00:00:00Z") > t("2026-09-30T23:59:59Z"));
    }

    #[test]
    fn knows_leap_years() {
        t("2028-02-29T00:00:00Z");
        t("2000-02-29T00:00:00Z");
        for bad in ["2026-02-29T00:00:00Z", "2100-02-29T00:00:00Z"] {
            assert!(bad.parse::<Timestamp>().is_err(), "{bad:?} was accepted");
        }
    }

    #[test]
    fn rejects_other_forms_and_impossible_times() {
        for bad in [
            "2026-11-23 12:00:00Z",
            "2026-11-23T12:00:00",
            "2026-11-23T12:00:00z",
            "2026-11-23T12:00:00+01:00",
            "2026-11-23T12:00:00.5Z",
            "2026-1-23T12:00:00Z",
            "2026-13-01T00:00:00Z",
            "2026-04-31T00:00:00Z",
            "2026-11-00T00:00:00Z",
            "2026-11-23T24:00:00Z",
            "2026-11-23T12:60:00Z",
            "2026-11-23T12:00:60Z",
            "2026-11-2xT12:00:00Z",
        ] {
            assert!(bad.parse::<Timestamp>().is_err(), "{bad:?} was accepted");
        }
    }
}
