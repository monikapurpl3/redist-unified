//! The real [`Machine`]: Windows itself.
//!
//! Every question only reads, except [`Machine::run`], which runs the program
//! a `command` rule names.

use std::ffi::{OsStr, OsString, c_void};
use std::io;
use std::mem::size_of;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::ptr;
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    ERROR_BAD_EXE_FORMAT, ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_PATH_NOT_FOUND,
    ERROR_RESOURCE_DATA_NOT_FOUND, ERROR_RESOURCE_TYPE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR,
};
use windows_sys::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VS_FIXEDFILEINFO, VerQueryValueW,
};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_WOW64_32KEY, KEY_WOW64_64KEY, REG_DWORD,
    REG_EXPAND_SZ, REG_QWORD, REG_ROUTINE_FLAGS, REG_SAM_FLAGS, REG_SZ, REG_VALUE_TYPE,
    RRF_NOEXPAND, RRF_RT_ANY, RRF_SUBKEY_WOW6432KEY, RRF_SUBKEY_WOW6464KEY, RegCloseKey,
    RegGetValueW, RegOpenKeyExW,
};
use windows_sys::Win32::System::SystemInformation::{
    IMAGE_FILE_MACHINE_AMD64, IMAGE_FILE_MACHINE_ARM64,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, IsWow64Process2};
use windows_sys::Win32::UI::Shell::{
    FOLDERID_ProgramFiles, FOLDERID_ProgramFilesX86, FOLDERID_System, FOLDERID_SystemX86,
    FOLDERID_Windows, KF_FLAG_DEFAULT, SHGetKnownFolderPath,
};

use crate::detection::{Machine, NativeArch, RegistryValue};
use crate::manifest::{KnownFolder, RegistryView};
use crate::version::Version;

/// How long a `command` rule's program may run before it is stopped.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

/// `CREATE_NO_WINDOW`: a console program run for a `command` rule gets no
/// window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The signature every `VS_FIXEDFILEINFO` starts with.
const FIXED_FILE_INFO_SIGNATURE: u32 = 0xFEEF_04BD;

/// The Windows the client runs on.
#[derive(Debug)]
pub struct WindowsMachine {
    native: NativeArch,
}

impl WindowsMachine {
    /// Asks Windows what it runs on. Fails on anything but x64 and ARM64
    /// Windows.
    pub fn new() -> io::Result<Self> {
        let mut process = 0;
        let mut native = 0;
        // SAFETY: GetCurrentProcess returns a pseudo handle that needs no
        // closing, and both out-pointers point at live u16s.
        if unsafe { IsWow64Process2(GetCurrentProcess(), &mut process, &mut native) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let native = match native {
            IMAGE_FILE_MACHINE_AMD64 => NativeArch::X64,
            IMAGE_FILE_MACHINE_ARM64 => NativeArch::Arm64,
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!("Windows for machine type {other:#06x} is not supported"),
                ));
            }
        };
        Ok(WindowsMachine { native })
    }
}

impl Machine for WindowsMachine {
    fn native_arch(&self) -> NativeArch {
        self.native
    }

    fn known_folder(&self, folder: KnownFolder) -> io::Result<PathBuf> {
        let id = match folder {
            KnownFolder::Windows => &FOLDERID_Windows,
            KnownFolder::System32 => &FOLDERID_System,
            KnownFolder::SysWow64 => &FOLDERID_SystemX86,
            KnownFolder::ProgramFiles => &FOLDERID_ProgramFiles,
            KnownFolder::ProgramFilesX86 => &FOLDERID_ProgramFilesX86,
        };
        let mut raw = ptr::null_mut();
        // SAFETY: id points at a GUID constant, a null token means the
        // current user, and raw is a valid out-pointer.
        let result =
            unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT as u32, ptr::null_mut(), &mut raw) };
        let path = if result >= 0 {
            // SAFETY: on success raw is a NUL-terminated UTF-16 string.
            let units = unsafe {
                let len = (0..).take_while(|&i| *raw.add(i) != 0).count();
                std::slice::from_raw_parts(raw, len)
            };
            Ok(PathBuf::from(OsString::from_wide(units)))
        } else {
            Err(io::Error::from_raw_os_error(result))
        };
        // SAFETY: the string must be freed whether or not the call
        // succeeded; freeing null is allowed.
        unsafe { CoTaskMemFree(raw.cast()) };
        path
    }

    fn registry_key_exists(&self, view: RegistryView, subkey: &str) -> io::Result<bool> {
        let subkey = wide(subkey);
        let mut key: HKEY = ptr::null_mut();
        // SAFETY: subkey is NUL-terminated and outlives the call, and key is
        // a valid out-pointer.
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_LOCAL_MACHINE,
                subkey.as_ptr(),
                0,
                KEY_QUERY_VALUE | sam_view(view),
                &mut key,
            )
        };
        match status {
            ERROR_SUCCESS => {
                // SAFETY: key was just opened and is closed once.
                unsafe { RegCloseKey(key) };
                Ok(true)
            }
            ERROR_FILE_NOT_FOUND => Ok(false),
            error => Err(win32_error(error)),
        }
    }

    fn registry_value(
        &self,
        view: RegistryView,
        subkey: &str,
        name: &str,
    ) -> io::Result<Option<RegistryValue>> {
        let subkey = wide(subkey);
        let name = wide(name);
        let flags = RRF_RT_ANY | RRF_NOEXPAND | routine_view(view);
        let mut data = vec![0u8; 256];
        loop {
            let mut kind: REG_VALUE_TYPE = 0;
            let mut size = u32::try_from(data.len()).unwrap_or(u32::MAX);
            // SAFETY: subkey and name are NUL-terminated and outlive the
            // call; data has room for size bytes; kind and size are valid
            // out-pointers.
            let status = unsafe {
                RegGetValueW(
                    HKEY_LOCAL_MACHINE,
                    subkey.as_ptr(),
                    name.as_ptr(),
                    flags,
                    &mut kind,
                    data.as_mut_ptr().cast(),
                    &mut size,
                )
            };
            match status {
                ERROR_SUCCESS => {
                    data.truncate(size as usize);
                    return Ok(Some(decode(kind, &data)));
                }
                // The value grew or was bigger than the buffer: size now
                // says how much it needs.
                ERROR_MORE_DATA => {
                    let needed = (size as usize).max(data.len() * 2);
                    data.resize(needed, 0);
                }
                ERROR_FILE_NOT_FOUND => return Ok(None),
                error => return Err(win32_error(error)),
            }
        }
    }

    fn file_exists(&self, path: &Path) -> io::Result<bool> {
        match std::fs::metadata(path) {
            Ok(metadata) => Ok(metadata.is_file()),
            Err(error) if is_missing(&error) => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn file_version(&self, path: &Path) -> io::Result<Option<Version>> {
        let path = wide(path);
        let mut ignored = 0;
        // SAFETY: path is NUL-terminated and outlives the call.
        let size = unsafe { GetFileVersionInfoSizeW(path.as_ptr(), &mut ignored) };
        if size == 0 {
            let error = io::Error::last_os_error();
            // A missing file, or one with no version resource (not a program
            // file, or one without the resource), has no version.
            return match error.raw_os_error().map(|code| code as WIN32_ERROR) {
                Some(
                    ERROR_FILE_NOT_FOUND
                    | ERROR_PATH_NOT_FOUND
                    | ERROR_RESOURCE_TYPE_NOT_FOUND
                    | ERROR_RESOURCE_DATA_NOT_FOUND
                    | ERROR_BAD_EXE_FORMAT,
                ) => Ok(None),
                _ => Err(error),
            };
        }
        let mut block = vec![0u8; size as usize];
        // SAFETY: block has room for size bytes, and path is NUL-terminated.
        if unsafe { GetFileVersionInfoW(path.as_ptr(), 0, size, block.as_mut_ptr().cast()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let root = wide("\\");
        let mut info: *mut c_void = ptr::null_mut();
        let mut len = 0;
        // SAFETY: block holds the version information just read, root is
        // NUL-terminated, and info and len are valid out-pointers.
        let found =
            unsafe { VerQueryValueW(block.as_ptr().cast(), root.as_ptr(), &mut info, &mut len) };
        if found == 0 || info.is_null() || (len as usize) < size_of::<VS_FIXEDFILEINFO>() {
            return Ok(None);
        }
        // SAFETY: VerQueryValueW pointed info into block, which is still
        // alive, at a VS_FIXEDFILEINFO of len bytes. Its alignment inside
        // block is not guaranteed, so it is read unaligned.
        let fixed = unsafe { info.cast::<VS_FIXEDFILEINFO>().read_unaligned() };
        if fixed.dwSignature != FIXED_FILE_INFO_SIGNATURE {
            return Ok(None);
        }
        Ok(Some(Version::from_parts([
            fixed.dwFileVersionMS >> 16,
            fixed.dwFileVersionMS & 0xFFFF,
            fixed.dwFileVersionLS >> 16,
            fixed.dwFileVersionLS & 0xFFFF,
        ])))
    }

    fn subfolders(&self, path: &Path) -> io::Result<Vec<String>> {
        let entries = match std::fs::read_dir(path) {
            Ok(entries) => entries,
            Err(error) if is_missing(&error) => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                // A name that is not valid Unicode is no version number.
                if let Ok(name) = entry.file_name().into_string() {
                    names.push(name);
                }
            }
        }
        Ok(names)
    }

    fn run(&self, program: &Path, args: &[String]) -> io::Result<u32> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()?;
        let started = Instant::now();
        loop {
            if let Some(status) = child.try_wait()? {
                // Windows exit codes are DWORDs; std hands them over as i32.
                return status
                    .code()
                    .map(|code| code as u32)
                    .ok_or_else(|| io::Error::other("the program ended without an exit code"));
            }
            if started.elapsed() > COMMAND_TIMEOUT {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "it was stopped after running for {} seconds",
                        COMMAND_TIMEOUT.as_secs()
                    ),
                ));
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}

/// A NUL-terminated UTF-16 copy of `text` for Windows.
fn wide(text: impl AsRef<OsStr>) -> Vec<u16> {
    text.as_ref().encode_wide().chain(Some(0)).collect()
}

fn win32_error(error: WIN32_ERROR) -> io::Error {
    io::Error::from_raw_os_error(error as i32)
}

/// Whether an error only says that a file or folder is not there.
fn is_missing(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

fn sam_view(view: RegistryView) -> REG_SAM_FLAGS {
    match view {
        RegistryView::Bits32 => KEY_WOW64_32KEY,
        RegistryView::Bits64 => KEY_WOW64_64KEY,
    }
}

fn routine_view(view: RegistryView) -> REG_ROUTINE_FLAGS {
    match view {
        RegistryView::Bits32 => RRF_SUBKEY_WOW6432KEY,
        RegistryView::Bits64 => RRF_SUBKEY_WOW6464KEY,
    }
}

fn decode(kind: REG_VALUE_TYPE, data: &[u8]) -> RegistryValue {
    match kind {
        REG_DWORD => match <[u8; 4]>::try_from(data) {
            Ok(bytes) => RegistryValue::Dword(u32::from_le_bytes(bytes)),
            Err(_) => RegistryValue::Other,
        },
        REG_QWORD => match <[u8; 8]>::try_from(data) {
            Ok(bytes) => RegistryValue::Qword(u64::from_le_bytes(bytes)),
            Err(_) => RegistryValue::Other,
        },
        REG_SZ | REG_EXPAND_SZ => {
            let (pairs, _) = data.as_chunks::<2>();
            let units: Vec<u16> = pairs.iter().map(|&pair| u16::from_le_bytes(pair)).collect();
            let end = units
                .iter()
                .position(|&unit| unit == 0)
                .unwrap_or(units.len());
            RegistryValue::String(String::from_utf16_lossy(&units[..end]))
        }
        _ => RegistryValue::Other,
    }
}

#[cfg(test)]
mod tests {
    //! These only read what every Windows 10 and 11 has, and run `cmd /c exit`.

    use std::path::PathBuf;

    use super::WindowsMachine;
    use crate::detection::{Machine, RegistryValue};
    use crate::manifest::KnownFolder;
    use crate::manifest::RegistryView::{Bits32, Bits64};

    const CURRENT_VERSION: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    const MISSING_KEY: &str = r"SOFTWARE\redist-unified\no such key";

    fn machine() -> WindowsMachine {
        WindowsMachine::new().unwrap()
    }

    fn lower(path: PathBuf) -> String {
        path.to_string_lossy().to_lowercase()
    }

    #[test]
    fn known_folders() {
        let m = machine();
        let name = |folder| {
            let path = m.known_folder(folder).unwrap();
            path.file_name().unwrap().to_string_lossy().to_lowercase()
        };
        assert_eq!(name(KnownFolder::System32), "system32");
        assert_eq!(name(KnownFolder::SysWow64), "syswow64");
        assert_eq!(name(KnownFolder::ProgramFiles), "program files");
        assert_eq!(name(KnownFolder::ProgramFilesX86), "program files (x86)");
        let system32 = m.known_folder(KnownFolder::System32).unwrap();
        assert_eq!(
            system32.parent().unwrap(),
            m.known_folder(KnownFolder::Windows).unwrap()
        );
    }

    #[test]
    fn registry_keys_and_values() {
        let m = machine();
        assert!(m.registry_key_exists(Bits64, CURRENT_VERSION).unwrap());
        assert!(!m.registry_key_exists(Bits64, MISSING_KEY).unwrap());
        assert!(!m.registry_key_exists(Bits32, MISSING_KEY).unwrap());
        assert_eq!(
            m.registry_value(Bits64, CURRENT_VERSION, "CurrentMajorVersionNumber")
                .unwrap(),
            Some(RegistryValue::Dword(10))
        );
        let Some(RegistryValue::String(build)) = m
            .registry_value(Bits64, CURRENT_VERSION, "CurrentBuild")
            .unwrap()
        else {
            panic!("CurrentBuild is not a string");
        };
        assert!(build.parse::<u32>().is_ok(), "{build:?}");
        assert_eq!(
            m.registry_value(Bits64, CURRENT_VERSION, "no such value")
                .unwrap(),
            None
        );
        assert_eq!(m.registry_value(Bits64, MISSING_KEY, "x").unwrap(), None);
    }

    #[test]
    fn registry_value_types() {
        let m = machine();
        let key = r"SOFTWARE\Microsoft\Windows\CurrentVersion";
        // REG_EXPAND_SZ comes back as written, not expanded.
        assert_eq!(
            m.registry_value(Bits64, key, "ProgramFilesPath").unwrap(),
            Some(RegistryValue::String("%ProgramFiles%".into()))
        );
        // REG_BINARY is none of the types rules compare.
        assert_eq!(
            m.registry_value(Bits64, CURRENT_VERSION, "DigitalProductId")
                .unwrap(),
            Some(RegistryValue::Other)
        );
    }

    #[test]
    fn registry_views_differ() {
        let m = machine();
        let key = r"SOFTWARE\Microsoft\Windows\CurrentVersion";
        let program_files = |view| match m.registry_value(view, key, "ProgramFilesDir").unwrap() {
            Some(RegistryValue::String(dir)) => dir.to_lowercase(),
            other => panic!("ProgramFilesDir is {other:?}"),
        };
        assert_eq!(
            program_files(Bits64),
            lower(m.known_folder(KnownFolder::ProgramFiles).unwrap())
        );
        assert_eq!(
            program_files(Bits32),
            lower(m.known_folder(KnownFolder::ProgramFilesX86).unwrap())
        );
    }

    #[test]
    fn files_and_versions() {
        let m = machine();
        let system32 = m.known_folder(KnownFolder::System32).unwrap();
        let kernel32 = system32.join("kernel32.dll");
        assert!(m.file_exists(&kernel32).unwrap());
        assert!(!m.file_exists(&system32).unwrap(), "a folder is not a file");
        assert!(!m.file_exists(&system32.join("no such file.dll")).unwrap());
        assert!(
            !m.file_exists(&system32.join(r"no such folder\file.dll"))
                .unwrap()
        );
        assert_eq!(m.file_version(&kernel32).unwrap().unwrap().major(), 10);
        assert_eq!(
            m.file_version(&system32.join("no such file.dll")).unwrap(),
            None
        );
        // A text file has no version resource.
        let win_ini = m
            .known_folder(KnownFolder::Windows)
            .unwrap()
            .join("win.ini");
        assert!(m.file_exists(&win_ini).unwrap());
        assert_eq!(m.file_version(&win_ini).unwrap(), None);
    }

    #[test]
    fn subfolders() {
        let m = machine();
        let windows = m.known_folder(KnownFolder::Windows).unwrap();
        let names = m.subfolders(&windows).unwrap();
        assert!(names.iter().any(|n| n.eq_ignore_ascii_case("System32")));
        assert!(!names.iter().any(|n| n.eq_ignore_ascii_case("win.ini")));
        assert!(
            m.subfolders(&windows.join("no such folder"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn exit_codes() {
        let m = machine();
        let cmd = m
            .known_folder(KnownFolder::System32)
            .unwrap()
            .join("cmd.exe");
        let run = |code: &str| m.run(&cmd, &["/c".into(), format!("exit {code}")]).unwrap();
        assert_eq!(run("0"), 0);
        assert_eq!(run("3010"), 3010);
        // Exit codes are DWORDs: -1 is 0xFFFFFFFF.
        assert_eq!(run("-1"), u32::MAX);
    }
}
