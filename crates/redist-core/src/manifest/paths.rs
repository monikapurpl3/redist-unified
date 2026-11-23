//! Paths in the manifest: relative paths inside a folder, and paths that start
//! at a well-known Windows folder.

use std::fmt;

use serde::Deserialize;

/// A relative Windows path such as `DXSETUP.exe` or `sub\file.dll`.
///
/// Backslash separators only, no empty, `.` or `..` parts, and none of
/// `<>:"/|?*` or control characters, so it can never leave its folder.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct RelativePath(String);

impl RelativePath {
    /// The path as written in the manifest.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for RelativePath {
    type Error = String;

    fn try_from(path: String) -> Result<Self, String> {
        let fine = path.split('\\').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part
                    .chars()
                    .any(|c| c.is_control() || "<>:\"/|?*".contains(c))
        });
        if fine {
            Ok(RelativePath(path))
        } else {
            Err(format!(
                "{path:?} is not a relative Windows path (backslashes, no empty, . or .. parts, none of <>:\"/|?*)"
            ))
        }
    }
}

impl fmt::Display for RelativePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A folder the client asks Windows for, never the environment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KnownFolder {
    /// `{windows}`: the Windows folder.
    Windows,
    /// `{system32}`: the native system folder.
    System32,
    /// `{syswow64}`: the 32-bit system folder of 64-bit Windows.
    SysWow64,
    /// `{program_files}`: the native Program Files.
    ProgramFiles,
    /// `{program_files_x86}`: the 32-bit Program Files of 64-bit Windows.
    ProgramFilesX86,
}

impl KnownFolder {
    const ALL: [KnownFolder; 5] = [
        KnownFolder::Windows,
        KnownFolder::System32,
        KnownFolder::SysWow64,
        KnownFolder::ProgramFiles,
        KnownFolder::ProgramFilesX86,
    ];

    /// The name between the braces.
    pub fn name(self) -> &'static str {
        match self {
            KnownFolder::Windows => "windows",
            KnownFolder::System32 => "system32",
            KnownFolder::SysWow64 => "syswow64",
            KnownFolder::ProgramFiles => "program_files",
            KnownFolder::ProgramFilesX86 => "program_files_x86",
        }
    }
}

/// A path that starts at a known folder, such as `{system32}\d3dx9_43.dll`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct KnownPath {
    folder: KnownFolder,
    relative: RelativePath,
}

impl KnownPath {
    /// The folder the path starts at.
    pub fn folder(&self) -> KnownFolder {
        self.folder
    }

    /// The rest of the path, inside the folder.
    pub fn relative(&self) -> &RelativePath {
        &self.relative
    }
}

impl TryFrom<String> for KnownPath {
    type Error = String;

    fn try_from(path: String) -> Result<Self, String> {
        let known = KnownFolder::ALL.map(KnownFolder::name).join("}, {");
        let Some((name, rest)) = path.strip_prefix('{').and_then(|p| p.split_once('}')) else {
            return Err(format!(
                "{path:?} does not start with a known folder ({{{known}}})"
            ));
        };
        let Some(folder) = KnownFolder::ALL.into_iter().find(|f| f.name() == name) else {
            return Err(format!(
                "{path:?} starts with an unknown folder; the known ones are {{{known}}}"
            ));
        };
        let Some(relative) = rest.strip_prefix('\\') else {
            return Err(format!("{path:?} needs a \\ and a path after {{{name}}}"));
        };
        let relative =
            RelativePath::try_from(relative.to_owned()).map_err(|e| format!("in {path:?}: {e}"))?;
        Ok(KnownPath { folder, relative })
    }
}

impl fmt::Display for KnownPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{{{}}}\\{}", self.folder.name(), self.relative)
    }
}

#[cfg(test)]
mod tests {
    use super::{KnownFolder, KnownPath, RelativePath};

    #[test]
    fn known_paths_split_into_folder_and_rest() {
        let path = KnownPath::try_from(r"{syswow64}\d3dx9_43.dll".to_owned()).unwrap();
        assert_eq!(path.folder(), KnownFolder::SysWow64);
        assert_eq!(path.relative().as_str(), "d3dx9_43.dll");
        assert_eq!(path.to_string(), r"{syswow64}\d3dx9_43.dll");
    }

    #[test]
    fn relative_paths_stay_inside_their_folder() {
        for good in ["DXSETUP.exe", r"sub\file.dll", "{GUID}.txt"] {
            assert!(RelativePath::try_from(good.to_owned()).is_ok(), "{good:?}");
        }
        for bad in [
            "",
            r"..\x.exe",
            r"a\..\x.exe",
            r".\x.exe",
            r"\x.exe",
            r"a\\x.exe",
            "a/x.exe",
            r"C:\x.exe",
            "x.exe:stream",
        ] {
            assert!(RelativePath::try_from(bad.to_owned()).is_err(), "{bad:?}");
        }
    }
}
