//! Download locations and their hashes.

use std::fmt;

use serde::Deserialize;

/// An `https://` URL. The client never downloads over plain HTTP.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct HttpsUrl(String);

impl HttpsUrl {
    /// The URL as written in the manifest.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for HttpsUrl {
    type Error = String;

    fn try_from(url: String) -> Result<Self, String> {
        let Some(rest) = url.strip_prefix("https://") else {
            return Err(format!("URL {url:?} is not https://"));
        };
        let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
        let clean = !url
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '\\');
        if host.is_empty() || host.contains('@') || !clean || url.len() > 2048 {
            return Err(format!(
                "URL {url:?} is not a plain https:// URL with a host"
            ));
        }
        Ok(HttpsUrl(url))
    }
}

impl fmt::Display for HttpsUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A SHA-256 digest, written in the manifest as 64 hex digits.
#[derive(Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct Sha256([u8; 32]);

impl Sha256 {
    /// The digest.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl TryFrom<String> for Sha256 {
    type Error = String;

    fn try_from(hex: String) -> Result<Self, String> {
        if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("sha256 {hex:?} is not 64 hex digits"));
        }
        let mut digest = [0; 32];
        let (pairs, _) = hex.as_bytes().as_chunks::<2>();
        for (byte, &[high, low]) in digest.iter_mut().zip(pairs) {
            *byte = (hex_value(high) << 4) | hex_value(low);
        }
        Ok(Sha256(digest))
    }
}

/// The value of one hex digit, already known to be one.
fn hex_value(digit: u8) -> u8 {
    match digit {
        b'0'..=b'9' => digit - b'0',
        b'a'..=b'f' => digit - b'a' + 10,
        _ => digit - b'A' + 10,
    }
}

impl fmt::Display for Sha256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Sha256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sha256({self})")
    }
}

#[cfg(test)]
mod tests {
    use super::{HttpsUrl, Sha256};

    #[test]
    fn sha256_reads_either_case_and_prints_lowercase() {
        let upper = "E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855";
        let digest = Sha256::try_from(upper.to_owned()).unwrap();
        assert_eq!(digest.to_string(), upper.to_ascii_lowercase());
        assert_eq!(digest.as_bytes()[0], 0xe3);
        assert_eq!(digest.as_bytes()[31], 0x55);
    }

    #[test]
    fn urls_must_be_plain_https() {
        for good in [
            "https://download.example/file.exe",
            "https://download.example",
        ] {
            assert!(HttpsUrl::try_from(good.to_owned()).is_ok(), "{good:?}");
        }
        for bad in [
            "http://download.example/file.exe",
            "HTTPS://download.example/file.exe",
            "https:///file.exe",
            "https://user@download.example/file.exe",
            "https://download.example/a file.exe",
            "https://download.example\\file.exe",
        ] {
            assert!(HttpsUrl::try_from(bad.to_owned()).is_err(), "{bad:?}");
        }
    }
}
