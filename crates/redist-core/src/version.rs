//! Dotted numeric versions such as `8.0.11` and `14.44.35211.0`.

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::str::FromStr;

use serde::Deserialize;

/// A version of one to four dot-separated numbers.
///
/// Comparison is numeric, part by part, and missing parts count as zero, so
/// `8.0` equals `8.0.0` and `14.44.35211.0` is newer than `14.44.9999.0`.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(try_from = "String")]
pub struct Version {
    parts: [u32; 4],
    len: u8,
}

impl Version {
    /// A four-part version, such as a file version resource.
    pub fn from_parts(parts: [u32; 4]) -> Version {
        Version { parts, len: 4 }
    }

    /// The first part.
    pub fn major(&self) -> u32 {
        self.parts[0]
    }

    /// The parts as written, without the implied zeros.
    pub fn parts(&self) -> &[u32] {
        &self.parts[..usize::from(self.len)]
    }
}

impl FromStr for Version {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        let mut parts = [0; 4];
        let mut len = 0;
        for part in text.split('.') {
            if len == parts.len() {
                return Err(format!("version {text:?} has more than four parts"));
            }
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return Err(format!("version {text:?} is not dot-separated numbers"));
            }
            parts[len] = part
                .parse()
                .map_err(|_| format!("version {text:?} has a part that is too large"))?;
            len += 1;
        }
        Ok(Version {
            parts,
            len: len as u8,
        })
    }
}

impl TryFrom<String> for Version {
    type Error = String;

    fn try_from(text: String) -> Result<Self, String> {
        text.parse()
    }
}

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.parts == other.parts
    }
}

impl Eq for Version {}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.parts.cmp(&other.parts)
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Hash for Version {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.parts.hash(state);
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, part) in self.parts().iter().enumerate() {
            if i > 0 {
                f.write_str(".")?;
            }
            write!(f, "{part}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Version;

    fn v(text: &str) -> Version {
        text.parse().unwrap()
    }

    #[test]
    fn missing_parts_count_as_zero() {
        assert_eq!(v("8.0"), v("8.0.0"));
        assert_eq!(v("8"), v("8.0.0.0"));
        assert!(v("8.0.1") > v("8.0"));
    }

    #[test]
    fn parts_compare_as_numbers() {
        assert!(v("14.44.35211.0") > v("14.44.9999.0"));
        assert!(v("8.0.11") > v("8.0.2"));
        assert!(v("10.0") > v("9.9.9.9"));
    }

    #[test]
    fn display_keeps_the_written_parts() {
        assert_eq!(v("8.0").to_string(), "8.0");
        assert_eq!(v("014.1").to_string(), "14.1");
        assert_eq!(v("14.44.35211.0").parts(), [14, 44, 35211, 0]);
        assert_eq!(v("8.0.11").major(), 8);
    }

    #[test]
    fn rejects_anything_else() {
        for bad in [
            "",
            ".",
            "8.",
            ".8",
            "8..0",
            "1.2.3.4.5",
            "v8",
            "8.0-rc.1",
            "+8",
            " 8",
            "99999999999",
        ] {
            assert!(bad.parse::<Version>().is_err(), "{bad:?} was accepted");
        }
    }
}
