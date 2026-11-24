//! The run's work folder, which only administrators and SYSTEM can change.
//!
//! The client runs elevated and starts what it downloaded. If downloads sat
//! in a folder the user's other programs can write to, any of them could
//! swap a checked installer for another, or put a DLL beside it, between the
//! hash check and the start. So each run gets a new folder under
//! `%SystemRoot%\Temp`, created in one call with a security descriptor that
//! gives:
//! - SYSTEM and Administrators full control;
//! - Users read access, so the log can be opened without elevation;
//! - the owner nothing more than that (`OWNER RIGHTS`), so a non-elevated
//!   process of the same user cannot rewrite the permissions;
//! - a High integrity label with no-write-up, which keeps every
//!   medium-integrity process from writing even where an ACE would allow it.
//!
//! The name is random and never reused: if it exists, another is tried.
//! `%SystemRoot%\Temp` lets users create folders, but not rename or delete
//! anyone else's.

use std::hash::{BuildHasher, Hasher, RandomState};
use std::io;
use std::mem::size_of;
use std::path::PathBuf;
use std::ptr;
use std::time::{SystemTime, UNIX_EPOCH};

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_ELEVATION, TOKEN_QUERY,
    TokenElevation,
};
use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use super::{known_folder_path, wide};
use crate::manifest::KnownFolder;

/// SYSTEM and Administrators: full control. Users and the owner: read and
/// execute. Inherited by everything inside.
const ACCESS: &str =
    "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;BU)(A;OICI;0x1200a9;;;OW)";

/// High integrity, no write up. Inherited by everything inside.
const LABEL: &str = "S:(ML;OICI;NW;;;HI)";

/// Creates the folder for one run. Needs elevation.
pub fn create_work_dir() -> io::Result<PathBuf> {
    if !is_elevated()? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the work folder can only be made with administrator rights",
        ));
    }
    let temp = known_folder_path(KnownFolder::Windows)?.join("Temp");
    let descriptor = Descriptor::from_sddl(&format!("{ACCESS}{LABEL}"))?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    for _ in 0..16 {
        let path = temp.join(format!("redist-unified-{:016x}", random()));
        let text = wide(&path);
        // SAFETY: text is NUL-terminated, and attributes points at a
        // descriptor that outlives the call.
        if unsafe { CreateDirectoryW(text.as_ptr(), &attributes) } != 0 {
            return Ok(path);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_ALREADY_EXISTS as i32) {
            return Err(error);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free name for the work folder",
    ))
}

/// Whether this process runs elevated.
pub fn is_elevated() -> io::Result<bool> {
    let mut token = ptr::null_mut();
    // SAFETY: the pseudo handle needs no closing; token is a valid
    // out-pointer.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut size = 0;
    // SAFETY: elevation has room for the TOKEN_ELEVATION asked for.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut size,
        )
    };
    let error = io::Error::last_os_error();
    // SAFETY: token was opened above and is closed once.
    unsafe { CloseHandle(token) };
    if ok == 0 {
        return Err(error);
    }
    Ok(elevation.TokenIsElevated != 0)
}

/// A security descriptor made from SDDL, freed when dropped.
struct Descriptor(PSECURITY_DESCRIPTOR);

impl Descriptor {
    fn from_sddl(sddl: &str) -> io::Result<Descriptor> {
        let text = wide(sddl);
        let mut descriptor = ptr::null_mut();
        // SAFETY: text is NUL-terminated; descriptor receives memory that
        // Drop frees with LocalFree.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Descriptor(descriptor))
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: the descriptor came from LocalAlloc and is freed once.
        unsafe { LocalFree(self.0) };
    }
}

/// A number for a folder name: from the process's random hash keys, the
/// time and the process id.
fn random() -> u64 {
    let mut hasher = RandomState::new().build_hasher();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    hasher.write_u128(now);
    hasher.write_u32(std::process::id());
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::{ACCESS, Descriptor, LABEL, create_work_dir, is_elevated};

    #[test]
    fn the_security_descriptor_is_valid() {
        Descriptor::from_sddl(&format!("{ACCESS}{LABEL}")).unwrap();
        assert!(Descriptor::from_sddl("D:(nonsense)").is_err());
    }

    #[test]
    fn the_work_folder_needs_elevation() {
        if is_elevated().unwrap() {
            // Elevated test runs make a real folder, and can delete it.
            let dir = create_work_dir().unwrap();
            std::fs::write(dir.join("probe.txt"), "x").unwrap();
            std::fs::remove_dir_all(&dir).unwrap();
        } else {
            let error = create_work_dir().unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        }
    }
}
