//! Downloads through WinHTTP, Windows' own HTTP stack: the system's proxy
//! settings and certificate store, and no TLS library in the client.

use std::ffi::c_void;
use std::fs::{self, File};
use std::io::{self, Write};
use std::mem::size_of;
use std::path::Path;
use std::ptr;

use windows_sys::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
use windows_sys::Win32::Networking::WinHttp::{
    ERROR_WINHTTP_AUTO_PROXY_SERVICE_ERROR, ERROR_WINHTTP_BAD_AUTO_PROXY_SCRIPT,
    ERROR_WINHTTP_CANNOT_CONNECT, ERROR_WINHTTP_CONNECTION_ERROR, ERROR_WINHTTP_HEADER_NOT_FOUND,
    ERROR_WINHTTP_INVALID_SERVER_RESPONSE, ERROR_WINHTTP_NAME_NOT_RESOLVED,
    ERROR_WINHTTP_REDIRECT_FAILED, ERROR_WINHTTP_SECURE_CERT_CN_INVALID,
    ERROR_WINHTTP_SECURE_CERT_DATE_INVALID, ERROR_WINHTTP_SECURE_CERT_REV_FAILED,
    ERROR_WINHTTP_SECURE_CERT_REVOKED, ERROR_WINHTTP_SECURE_CERT_WRONG_USAGE,
    ERROR_WINHTTP_SECURE_CHANNEL_ERROR, ERROR_WINHTTP_SECURE_FAILURE,
    ERROR_WINHTTP_SECURE_INVALID_CA, ERROR_WINHTTP_SECURE_INVALID_CERT, ERROR_WINHTTP_TIMEOUT,
    ERROR_WINHTTP_UNABLE_TO_DOWNLOAD_SCRIPT, URL_COMPONENTS, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
    WINHTTP_FLAG_SECURE, WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2, WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_3,
    WINHTTP_INTERNET_SCHEME_HTTPS, WINHTTP_OPTION_SECURE_PROTOCOLS, WINHTTP_QUERY_CONTENT_LENGTH,
    WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect, WinHttpCrackUrl, WinHttpOpen,
    WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
    WinHttpSendRequest, WinHttpSetOption, WinHttpSetTimeouts,
};

use super::wide;
use crate::manifest::HttpsUrl;
use crate::session::Fetcher;

const USER_AGENT: &str = concat!("redist-unified/", env!("CARGO_PKG_VERSION"));

/// A WinHTTP handle, closed when dropped.
struct Handle(*mut c_void);

impl Handle {
    fn new(raw: *mut c_void) -> io::Result<Handle> {
        if raw.is_null() {
            Err(last_error())
        } else {
            Ok(Handle(raw))
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle came from WinHTTP and is closed once.
        unsafe { WinHttpCloseHandle(self.0) };
    }
}

/// Downloads over HTTPS with WinHTTP. Redirects are followed, except from
/// HTTPS to plain HTTP (WinHTTP's default).
pub struct WinHttpFetcher {
    session: Handle,
}

// SAFETY: a WinHTTP session handle may be used from several threads at once;
// each download opens its own connection and request handles on it.
unsafe impl Send for WinHttpFetcher {}
unsafe impl Sync for WinHttpFetcher {}

impl WinHttpFetcher {
    /// Opens a WinHTTP session that uses the system's proxy settings.
    pub fn new() -> io::Result<Self> {
        let agent = wide(USER_AGENT);
        // SAFETY: agent is NUL-terminated; automatic proxy takes no proxy
        // names.
        let session = Handle::new(unsafe {
            WinHttpOpen(
                agent.as_ptr(),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                ptr::null(),
                ptr::null(),
                0,
            )
        })?;
        // Resolve, connect and send within 30 seconds each, and get each
        // part of the answer within 60.
        // SAFETY: session is a valid handle.
        if unsafe { WinHttpSetTimeouts(session.0, 30_000, 30_000, 30_000, 60_000) } == 0 {
            return Err(last_error());
        }
        // TLS 1.2 and 1.3 only. A Windows that does not know TLS 1.3
        // refuses the pair, so it gets TLS 1.2 alone.
        let both = WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2 | WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_3;
        if !set_protocols(&session, both)
            && !set_protocols(&session, WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2)
        {
            return Err(last_error());
        }
        Ok(WinHttpFetcher { session })
    }

    fn download(
        &self,
        url: &HttpsUrl,
        dest: &Path,
        progress: &dyn Fn(u64, Option<u64>),
    ) -> io::Result<()> {
        let target = Target::of(url)?;
        // SAFETY: host is NUL-terminated and outlives the call.
        let connection = Handle::new(unsafe {
            WinHttpConnect(self.session.0, target.host.as_ptr(), target.port, 0)
        })?;
        let verb = wide("GET");
        // SAFETY: verb and object are NUL-terminated; null version, referrer
        // and accept types mean WinHTTP's defaults.
        let request = Handle::new(unsafe {
            WinHttpOpenRequest(
                connection.0,
                verb.as_ptr(),
                target.object.as_ptr(),
                ptr::null(),
                ptr::null(),
                ptr::null(),
                WINHTTP_FLAG_SECURE,
            )
        })?;
        // SAFETY: request is valid; there are no extra headers and no body.
        if unsafe { WinHttpSendRequest(request.0, ptr::null(), 0, ptr::null(), 0, 0, 0) } == 0 {
            return Err(last_error());
        }
        // SAFETY: request is valid; the reserved pointer must be null.
        if unsafe { WinHttpReceiveResponse(request.0, ptr::null_mut()) } == 0 {
            return Err(last_error());
        }
        let status = query_number(&request, WINHTTP_QUERY_STATUS_CODE)?.unwrap_or(0);
        if status != 200 {
            return Err(io::Error::other(format!(
                "the server answered HTTP {status}"
            )));
        }
        let total = query_number(&request, WINHTTP_QUERY_CONTENT_LENGTH)?;

        let mut file = File::create_new(dest)?;
        let mut buffer = vec![0u8; 64 * 1024];
        let mut received = 0u64;
        progress(0, total);
        loop {
            let mut read = 0u32;
            // SAFETY: buffer has room for its whole length.
            let ok = unsafe {
                WinHttpReadData(
                    request.0,
                    buffer.as_mut_ptr().cast(),
                    buffer.len() as u32,
                    &mut read,
                )
            };
            if ok == 0 {
                return Err(last_error());
            }
            if read == 0 {
                break;
            }
            file.write_all(&buffer[..read as usize])?;
            received += u64::from(read);
            progress(received, total);
        }
        if let Some(total) = total
            && received != total
        {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("the download stopped after {received} of {total} bytes"),
            ));
        }
        file.flush()
    }
}

impl Fetcher for WinHttpFetcher {
    fn fetch(
        &self,
        url: &HttpsUrl,
        dest: &Path,
        progress: &dyn Fn(u64, Option<u64>),
    ) -> io::Result<()> {
        let result = self.download(url, dest, progress);
        if result.is_err() {
            let _ = fs::remove_file(dest);
        }
        result
    }
}

fn set_protocols(session: &Handle, protocols: u32) -> bool {
    // SAFETY: the option takes a u32, passed by pointer with its size.
    unsafe {
        WinHttpSetOption(
            session.0,
            WINHTTP_OPTION_SECURE_PROTOCOLS,
            (&protocols as *const u32).cast(),
            size_of::<u32>() as u32,
        ) != 0
    }
}

/// A numeric header, or `None` if the answer does not have it.
fn query_number(request: &Handle, info: u32) -> io::Result<Option<u64>> {
    match query_text(request, info)? {
        None => Ok(None),
        Some(text) => text.trim().parse().map(Some).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("the server sent {text:?} where a number belongs"),
            )
        }),
    }
}

/// A header as text, or `None` if the answer does not have it.
fn query_text(request: &Handle, info: u32) -> io::Result<Option<String>> {
    let mut size = 0u32;
    // SAFETY: a null buffer asks for the size the header needs.
    let ok = unsafe {
        WinHttpQueryHeaders(
            request.0,
            info,
            ptr::null(),
            ptr::null_mut(),
            &mut size,
            ptr::null_mut(),
        )
    };
    if ok != 0 {
        return Ok(Some(String::new()));
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error().map(|code| code as u32) {
        Some(ERROR_WINHTTP_HEADER_NOT_FOUND) => return Ok(None),
        Some(ERROR_INSUFFICIENT_BUFFER) => {}
        _ => return Err(readable(error)),
    }
    let mut buffer = vec![0u16; (size as usize).div_ceil(2)];
    // SAFETY: buffer has room for size bytes.
    let ok = unsafe {
        WinHttpQueryHeaders(
            request.0,
            info,
            ptr::null(),
            buffer.as_mut_ptr().cast(),
            &mut size,
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(last_error());
    }
    // On success, size is the length in bytes without the NUL.
    buffer.truncate(size as usize / 2);
    Ok(Some(String::from_utf16_lossy(&buffer)))
}

/// The last error, in words. WinHTTP's messages are not in the system's
/// message table, so Rust cannot print them.
fn last_error() -> io::Error {
    readable(io::Error::last_os_error())
}

fn readable(error: io::Error) -> io::Error {
    let Some(code) = error.raw_os_error() else {
        return error;
    };
    let (kind, text) = match code as u32 {
        ERROR_WINHTTP_TIMEOUT => (io::ErrorKind::TimedOut, "the server did not answer in time"),
        ERROR_WINHTTP_NAME_NOT_RESOLVED => (
            io::ErrorKind::NotFound,
            "the server's name could not be found",
        ),
        ERROR_WINHTTP_CANNOT_CONNECT => (
            io::ErrorKind::ConnectionRefused,
            "could not connect to the server",
        ),
        ERROR_WINHTTP_CONNECTION_ERROR => {
            (io::ErrorKind::ConnectionReset, "the connection was broken")
        }
        ERROR_WINHTTP_INVALID_SERVER_RESPONSE => (
            io::ErrorKind::InvalidData,
            "the server's answer was not valid",
        ),
        ERROR_WINHTTP_REDIRECT_FAILED => (io::ErrorKind::Other, "a redirect could not be followed"),
        ERROR_WINHTTP_SECURE_INVALID_CA
        | ERROR_WINHTTP_SECURE_INVALID_CERT
        | ERROR_WINHTTP_SECURE_CERT_CN_INVALID
        | ERROR_WINHTTP_SECURE_CERT_DATE_INVALID
        | ERROR_WINHTTP_SECURE_CERT_REVOKED
        | ERROR_WINHTTP_SECURE_CERT_REV_FAILED
        | ERROR_WINHTTP_SECURE_CERT_WRONG_USAGE => (
            io::ErrorKind::Other,
            "the server's certificate was not accepted",
        ),
        ERROR_WINHTTP_SECURE_FAILURE | ERROR_WINHTTP_SECURE_CHANNEL_ERROR => {
            (io::ErrorKind::Other, "the secure connection failed")
        }
        ERROR_WINHTTP_AUTO_PROXY_SERVICE_ERROR
        | ERROR_WINHTTP_BAD_AUTO_PROXY_SCRIPT
        | ERROR_WINHTTP_UNABLE_TO_DOWNLOAD_SCRIPT => {
            (io::ErrorKind::Other, "the proxy settings could not be used")
        }
        12_000..=12_199 => (io::ErrorKind::Other, "the download failed"),
        _ => return error,
    };
    io::Error::new(kind, format!("{text} (WinHTTP error {code})"))
}

/// Where a request goes: host, port, and path with query, all ready for
/// WinHTTP.
#[derive(Debug, PartialEq, Eq)]
struct Target {
    host: Vec<u16>,
    port: u16,
    object: Vec<u16>,
}

impl Target {
    fn of(url: &HttpsUrl) -> io::Result<Target> {
        // A fragment is never sent to the server.
        let text = url.as_str().split('#').next().unwrap_or_default();
        let text = wide(text);
        let mut parts = URL_COMPONENTS {
            dwStructSize: size_of::<URL_COMPONENTS>() as u32,
            // Nonzero lengths with null pointers ask for pointers into text.
            dwHostNameLength: u32::MAX,
            dwUrlPathLength: u32::MAX,
            dwExtraInfoLength: u32::MAX,
            ..URL_COMPONENTS::default()
        };
        // SAFETY: text is NUL-terminated (length 0 means "up to the NUL"),
        // and parts is set up to receive pointers into it.
        if unsafe { WinHttpCrackUrl(text.as_ptr(), 0, 0, &mut parts) } == 0 {
            return Err(last_error());
        }
        if parts.nScheme != WINHTTP_INTERNET_SCHEME_HTTPS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "only https:// is downloaded",
            ));
        }
        let piece = |start: *mut u16, len: u32| -> Vec<u16> {
            if start.is_null() || len == 0 {
                Vec::new()
            } else {
                // SAFETY: WinHttpCrackUrl pointed start into text, which is
                // still alive, with len units after it.
                unsafe { std::slice::from_raw_parts(start, len as usize) }.to_vec()
            }
        };
        let mut host = piece(parts.lpszHostName, parts.dwHostNameLength);
        host.push(0);
        let mut object = piece(parts.lpszUrlPath, parts.dwUrlPathLength);
        object.extend(piece(parts.lpszExtraInfo, parts.dwExtraInfoLength));
        if object.is_empty() {
            object.push(u16::from(b'/'));
        }
        object.push(0);
        Ok(Target {
            host,
            port: parts.nPort,
            object,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{Target, WinHttpFetcher, readable};
    use crate::manifest::HttpsUrl;
    use crate::session::{Fetcher, sha256_of};

    fn url(text: &str) -> HttpsUrl {
        HttpsUrl::try_from(text.to_owned()).unwrap()
    }

    fn text(units: &[u16]) -> String {
        String::from_utf16(units.strip_suffix(&[0]).unwrap()).unwrap()
    }

    #[test]
    fn urls_split_into_host_port_and_object() {
        let target = Target::of(&url("https://download.example/a/b.exe?x=1&y=2#part")).unwrap();
        assert_eq!(text(&target.host), "download.example");
        assert_eq!(target.port, 443);
        assert_eq!(text(&target.object), "/a/b.exe?x=1&y=2");

        let target = Target::of(&url("https://download.example:8443")).unwrap();
        assert_eq!(target.port, 8443);
        assert_eq!(text(&target.object), "/");
    }

    #[test]
    fn winhttp_errors_are_put_in_words() {
        let error = readable(std::io::Error::from_raw_os_error(12007));
        assert_eq!(
            error.to_string(),
            "the server's name could not be found (WinHTTP error 12007)"
        );
        assert_eq!(
            readable(std::io::Error::from_raw_os_error(12002)).kind(),
            std::io::ErrorKind::TimedOut
        );
        // Other errors keep Windows' own message.
        assert_eq!(
            readable(std::io::Error::from_raw_os_error(5)).raw_os_error(),
            Some(5)
        );
    }

    /// A fresh path in the temp folder, deleted afterwards.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("redist-http-{name}-{}", std::process::id()));
            let _ = std::fs::remove_file(&path);
            Scratch(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    // These reach the network (this project's own repository on GitHub),
    // so they only run when asked: cargo test -- --ignored

    #[test]
    #[ignore = "needs the network"]
    fn downloads_a_file() {
        let fetcher = WinHttpFetcher::new().unwrap();
        let dest = Scratch::new("license");
        let calls = std::cell::Cell::new(0);
        fetcher
            .fetch(
                &url("https://raw.githubusercontent.com/monikapurpl3/redist-unified/main/LICENSE"),
                &dest.0,
                &|_, _| calls.set(calls.get() + 1),
            )
            .unwrap();
        // The GPL-3.0 text, as committed.
        assert_eq!(
            sha256_of(&dest.0).unwrap().to_string(),
            "3972dc9744f6499f0f9b2dbf76696f2ae7ad8af9b23dde66d6af86c9dfb36986"
        );
        assert!(calls.get() >= 2);
    }

    #[test]
    #[ignore = "needs the network"]
    fn redirects_are_followed() {
        let fetcher = WinHttpFetcher::new().unwrap();
        let dest = Scratch::new("redirect");
        // github.com answers with a redirect to raw.githubusercontent.com.
        fetcher
            .fetch(
                &url("https://github.com/monikapurpl3/redist-unified/raw/main/LICENSE"),
                &dest.0,
                &|_, _| {},
            )
            .unwrap();
        assert_eq!(
            sha256_of(&dest.0).unwrap().to_string(),
            "3972dc9744f6499f0f9b2dbf76696f2ae7ad8af9b23dde66d6af86c9dfb36986"
        );
    }

    #[test]
    #[ignore = "needs the network"]
    fn a_missing_file_is_an_error_and_leaves_nothing() {
        let fetcher = WinHttpFetcher::new().unwrap();
        let dest = Scratch::new("missing");
        let error = fetcher
            .fetch(
                &url("https://raw.githubusercontent.com/monikapurpl3/redist-unified/main/no-such-file"),
                &dest.0,
                &|_, _| {},
            )
            .unwrap_err();
        assert!(error.to_string().contains("HTTP 404"), "{error}");
        assert!(!dest.0.exists());
    }
}
