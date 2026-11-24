# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

**If `PRIVATE.md` exists, read it first.** It is local only and never committed.
It holds the hosting and publishing setup, where the signing key is kept, and
details of the maintainer's machine. `redist-installer-plan.md` (the full design
plan) and `ASPIC.md` are local-only files as well. This file carries the public
part of the design.

## State of the project

The milestones, in the maintainer's order:
1. The manifest and detection library. Done.
2. The CLI installer. It is built and tested against fakes, and real downloads
   work. It has not installed anything for real yet, which needs a Windows VM
   and a manifest with checked URLs, hashes and arguments.
3. The UI.
4. The signed manifest.
5. The publish pipeline.
6. CI and SignPath.
7. Extras.

The UI comes once the core and CLI are done, before anything is signed (the
maintainer's decision, 2026-11-24).

## Commands

The project builds with **nightly**, which is the maintainer's choice and is
selected by `rust-toolchain.toml`. Quote the path in shell commands: the folder
name contains a space.

```bash
cargo build                                          # debug build
cargo build --release --locked                       # target/release/redist.exe
cargo test --locked                                  # every crate
cargo test -p redist-core <name>                     # tests whose name contains <name>
cargo test -p redist-core --test manifest            # one integration test file
cargo test -p redist-core --lib windows              # the real Windows backend (read-only)
cargo test -p redist-core --test download            # the download engine, against a fake server
cargo test -p redist-core --lib -- --ignored         # real downloads from this repo on GitHub
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --check
```

**Never start `redist.exe` on the development machine.** It requires
administrator rights and installs runtimes, so test it in a VM.
- From an unelevated shell, `cargo run` fails with "requires elevation"
  (os error 740).
- From PowerShell, even `redist.exe --help` raises a UAC prompt, because
  PowerShell hands programs that need elevation to the Windows shell.
- To check the exe without running it, read its manifest with `mt.exe`
  (Windows SDK):

  ```bash
  mt.exe -nologo -inputresource:target\release\redist.exe;#1 -out:redist.manifest
  ```

Keep the build free of warnings. Cargo warns about unused dependencies,
including unused `[workspace.dependencies]` entries, so add a dependency in the
same change as the code that uses it.

**Check the static CRT after changing the build setup.** Run
`dumpbin -nologo -dependents target/release/redist.exe`. The client must not
import `VCRUNTIME140.dll` or any `api-ms-win-crt-*` DLL, because installing
those is its job. Only DLLs that are part of Windows are expected: `kernel32`,
`ntdll`, `advapi32`, `shell32`, `combase`, `version`, `winhttp`,
`bcryptprimitives` and the `api-ms-win-core-*` API sets. A build without
`+crt-static` lists the CRT DLLs.

## What is being built

A single double-click Windows exe, written in Rust, that detects which common
runtimes are missing (VC++ 2005 to 2022, DirectX June 2010, .NET Desktop Runtime,
optionally .NET Framework 3.5) for x86 and x64 (ARM64 later), installs only
those, and prompts for a reboot once at the end. It is aimed at less
experienced users, so there are no choices to make.

### The core design rule: dumb client, smart manifest

Every version number, URL, hash, detection rule, silent-install argument and
exit-code mapping lives in a signed `manifest.json`. Adding or fixing a
component must only need a manifest change, never a client rebuild or
re-signing. If a change seems to need component-specific logic in the client,
add a generic manifest field or a detection type instead. The detection types
are `registry` (value comparison), `file` (exists or DLL version),
`dotnet_runtime` (version folders under `dotnet\shared\...`), and `command` as a
last resort.

Client flow: fetch the manifest and signature, verify them against the embedded
minisign (ed25519) public key plus the replay floor, detect, download the
missing components in parallel and check their hashes, install **one at a
time**, then prompt for a reboot once. Installs must be serial because Windows
Installer allows only one transaction at a time.

The per-component `exit_ok`, `exit_reboot`, `exit_already` and `exit_retry`
lists in the manifest map installer exit codes:

| Code | Meaning | What the client does |
|---|---|---|
| `0` | Success | Carry on |
| `3010` | Success, reboot needed | Remember it and prompt once at the end |
| `1641` | Reboot started | Treat like 3010. Pass `/norestart` to prevent it |
| `1638` | A newer version is already installed | Not an error |
| `1618` | Another install is running | Wait and retry |

A real failure stops the run with a plain-language message and the log path.
The client never carries on and reports success.

### Code layout

- `crates/redist-core` is the library: manifest, detection, and the install
  session. It is the real product.
- `crates/redist` is the thin binary (CLI, later the UI) that builds
  `redist.exe`.
  - `src/lib.rs` holds what has tests: the command line, and every line of text
    it prints.
  - `src/main.rs` wires up the Windows pieces.
  - For now `--manifest <file>` reads an unsigned local manifest, for
    development only. `--only <id>` installs one component, and `--check`
    only detects.
  - Exit codes: 0, 1 (failure), 2 (usage or manifest error), and 3010 (a
    restart is still needed).
- The repository is called `redist-unified`. The crate and exe names are
  working names and have not been renamed to match.
- `.cargo/config.toml` sets `+crt-static` for `x86_64-pc-windows-msvc` (the
  first target) and `aarch64-pc-windows-msvc` (later).
- **Application manifests.** `crates/redist/build.rs` embeds
  `requireAdministrator` plus `crates/redist-core/windows-compat.manifest` (the
  Windows 10/11 `supportedOS` GUID) through `rustc-link-arg-bins`.
  - That link argument also reaches the binary's own test build, which then
    cannot run unelevated. So the `[[bin]]` has `test = false`, and its tested
    code lives in the crate's library.
  - `crates/redist-core/build.rs` embeds the compatibility manifest alone
    (`asInvoker`) in the library's tests and examples. They see Windows as the
    client does, without elevation.
  - Without `supportedOS`, Windows reports its own files (`kernel32.dll`) as
    version 6.2. Runtime DLLs are not affected.
- `Cargo.lock` is committed, and every release build uses `--locked`.
- `.gitattributes` forces LF line endings.

### The manifest schema

The schema lives in `crates/redist-core/src/manifest.rs` and the modules under
`manifest/`. The rustdoc on each type is the reference for its JSON fields.
`tests/data/manifest.json` shows every installer and rule type.

- **Parsing is strict.** Every struct uses `deny_unknown_fields`, so a typo in
  the manifest fails instead of being ignored. Adding a field therefore needs a
  new client and a raised `min_client`.
- **Checks happen during deserialization,** so each error carries a line and
  column:
  - Value types check themselves through `try_from`: `Version`, `Timestamp`,
    `HttpsUrl`, `Sha256`, `RelativePath`, `KnownPath`, `RegistryKey`.
  - Rules that combine fields go through a private `Raw*` twin and a
    `TryFrom` (`RawComponent`, `RawRegistryRule`, `RawCommandRule`).
  - Only checks across components, such as unique ids, run after parsing, in
    `Manifest::parse`.
- `Manifest::parse` reads `schema` alone first, so a newer manifest reports
  "update the client" rather than an error about some field.
- **Registry rules:**
  - They are `HKLM` only.
  - They pick a `view` (`"32"` or `"64"`), and a key that names `WOW6432Node`
    is refused.
  - They take at most one of `min`, `equals` and `min_version`. With none, they
    check presence.
- **Paths:**
  - File and command paths start at a known folder (`{system32}`,
    `{syswow64}`, `{windows}`, `{program_files}`, `{program_files_x86}`),
    which the client resolves through Windows, never through the environment
    or `PATH`.
  - No path can contain `..`.
- **Installers:**
  - `exe` and `msi`.
  - `extract`, a self-extractor with `{dir}` in `extract_args`, then `run`
    inside that folder (DirectX).
  - `windows_feature` (DISM, NetFx3). It is the only installer without
    `urls`, `size` and `sha256`.
- **Downloads are metalinks in miniature.**
  - `urls` lists mirrors of one file, in order of preference: never empty,
    no duplicates.
  - `size` is its exact size in bytes. The client splits files by it, and
    refuses any server whose file is another size.
  - `sha256` covers the file whichever mirrors its parts came from.
  - Only .NET may have a mirror besides Microsoft's own URL (see the
    Invariants).
- **Arch:** `x86`, `x64` or `arm64`, plus `any` for components that are not tied
  to one architecture (DirectX, Windows features).
- **Exit codes:** `exit_ok`, `exit_reboot`, `exit_already` and `exit_retry`
  are all required, can be empty except `exit_ok`, and never share a code. A
  code in none of them means failure.
- **`latest_client`** is `version` and `url` only. The client never downloads
  itself, so it has no hash.
- **What `parse` leaves out:** it does not verify the signature and does not
  decide acceptance (replay protection, `min_client`). Both belong to
  milestone 4.
- **The fixture is not real data:** its URLs, sizes and hashes are dummies, and its
  silent arguments and detection values have not been checked against the real
  installers.
- A `dotnet_runtime` rule needs a concrete arch to find dotnet, so it is refused
  in an `any` component.

### Detection

`src/detection.rs` holds the logic:
- the `Machine` trait, which lists every question detection asks;
- `is_installed`;
- `applies_to`.

`src/windows.rs` (`WindowsMachine`, `windows-sys`) answers those questions for
real and holds the crate's only `unsafe` code. `tests/detection.rs` answers them
with a `FakeMachine`, which ignores case like Windows does.
`tests/common/mod.rs` holds the fixture helpers that both test files share.

- **When in doubt, "not installed".** Each of these fails its rule instead of
  raising an error:
  - a value of the wrong type;
  - an unparsable version;
  - a preview .NET folder;
  - an empty version folder left by an uninstall.

  Reinstalling is harmless, because the installer reports "already there".
  Skipping a missing runtime is not.
- **`Err` only when the machine cannot answer,** such as access denied. `all`
  and `any` treat an error as "unknown": a rule that settles the result wins
  over it.
- **The client is 64-bit only,** so `NativeArch` is `X64` or `Arm64`.
  `applies_to` puts ARM64 components on ARM64 only. x64 ones go on both, because
  emulated x64 programs need their own runtimes.
- **.NET roots:**
  - x86 uses `{program_files_x86}\dotnet`.
  - The native architecture uses `{program_files}\dotnet`.
  - x64 on ARM64 uses `{program_files}\dotnet\x64`.

  A runtime counts only when it has the same major version, is at least `min`,
  and has `<framework>.deps.json` in its folder.
- **Command rules** run only if the program exists, without a window and with a
  60-second limit, and look at nothing but the exit code.
- Paths are joined with `\` on every platform, so the fake and the real machine
  see the same strings.

### Installing: the session

`src/session.rs` runs the whole flow:
1. Detect.
2. Download every missing component, four at a time, each through the engine
   in `src/download.rs` (below).
3. Install them one by one, in manifest order.
4. Detect again after each one that reports plain success.

It reaches the outside world only through the `Fetcher`, `Runner`,
`Reporter` and `Log` it is given. `tests/session.rs` drives it with fakes, and
the `FakeMachine` in `tests/common` can change mid-test, so a fake installer
can "install" things.

- **The first failure stops the run.** Nothing after it installs, and the run
  never reports success.
  - Downloads all finish first. Then the first failure in manifest order, if
    any, is reported, and nothing is installed.
- **The SHA-256 is checked on the file on disk,** which is the one that will
  run. A mismatch is deleted and never run.
- **The download engine** (`src/download.rs`) works like aria2 with a
  metalink, inside the client. `tests/download.rs` drives it with a fake
  server whose mirrors misbehave on purpose.
  - **Parts.** A file of at least 2 MiB is split into up to 4 parts of at
    least 1 MiB, fetched in parallel. With 4 files at once, that is at most 16
    connections.
  - **Mirrors are a priority list, not load-sharing.** Every part starts on
    the first URL, and moves to the next one only when its mirror fails. This
    keeps load off fallbacks such as a small home server.
  - **Resume.** A part that stops early continues from its last byte, using
    a `Range` request.
  - **When to move on.** After 3 tries in a row that bring no new bytes, with
    waits of 1, 2 and 4 seconds, a part moves to the next mirror. A try that
    brought bytes starts the count again.
  - **Final errors.** The error kinds `NotFound`, `PermissionDenied`,
    `InvalidData` and `Unsupported` move a part to the next mirror at once.
    The `Fetcher` reports HTTP statuses, size mismatches and "no ranges" with
    these kinds.
  - **No ranges.** A mirror that ignores ranges is skipped for parts. If no
    mirror supports them, the whole file is fetched in one piece.
  - **Limits.** Bytes past a part's end are never written. A failed download
    leaves no file, and the destination must be new.
  - **Not yet.** Resuming across runs needs a download cache that outlives
    the run's private work folder. That cache would need the same protection
    as the work folder, so it is left for later.
- **Exit codes:**
  - 1618 waits 30 seconds and tries again, up to 10 times.
  - 3010 and 1641 skip the detection after installing, because what they
    install may only show after the restart.
  - 1638 counts as done.
  - Plain success that detection then does not see is a failure: the
    installer did nothing.
- **Commands the client builds:**
  - `msi` runs `{system32}\msiexec.exe /i <file> <args>`.
  - `extract` replaces `{dir}` with `<work>\<id>`, then runs `<run>` from
    that folder.
  - `windows_feature` runs `{system32}\dism.exe /Online /Enable-Feature
    /FeatureName:<f> /All /NoRestart`. These are the only installer arguments
    that do not come from the manifest.

The Windows backend lives in `src/windows/`:
- **`WinHttpFetcher`** fetches byte ranges through WinHTTP.
  - It uses the system proxy and certificate store and only TLS 1.2 and 1.3.
    It follows redirects, but never from HTTPS to HTTP, and the `Range`
    header survives the redirect.
  - It accepts a 206 only with the `Content-Range` it asked for and the
    manifest's size. It accepts a 200 only when it asked for the whole file;
    any other 200 is an `Unsupported` error.
  - Rust cannot print WinHTTP's error messages, which are not in the system
    message table, so `readable` puts them into words.
  - WinHTTP replaced the plan's `reqwest`. `reqwest` 0.13 needs `aws-lc-rs`,
    which takes CMake and NASM to build, and `tokio`.
- **`ProcessRunner`** runs installers without a window, from the work folder.
  - Each installer's console output goes to `<program>.output.txt` there.
  - It stops an installer after 60 minutes.
- **`create_work_dir`** makes `%SystemRoot%\Temp\redist-unified-<random>`.
  - It is created in one call with a descriptor: administrators and SYSTEM
    write, users and the owner only read, and a High integrity no-write-up
    label.
  - So no unelevated program can swap a checked installer or plant a DLL next
    to it. `workdir.rs` explains why.
  - The CLI deletes its subfolders at the end, and keeps the log and the
    installers' output.

## Invariants

- Microsoft installers are never repacked or modified. By default the client
  downloads them from Microsoft's own URLs, and only .NET (MIT) may be mirrored.
- The manifest signing key never enters the repository or CI.
- The client refuses a manifest whose `version` is older than the last one it
  saw, or whose `issued` is older than the floor built into the client.
- The publish pipeline checks Microsoft's Authenticode signatures
  (`osslsigncode verify`) and the hashes before anything goes into the manifest.
  For .NET, the SHA-512 hashes come from Microsoft's `releases.json`.
- Every silent-install argument is verified by hand before it goes into the
  manifest. The old VC++ packages (2005, 2008, 2010) use `/q`-style switches.
- Exe signing happens only in GitHub Actions through SignPath Foundation. Local
  builds are unsigned dev builds.
- The installer requires administrator rights and changes the system. Test it on
  clean Windows 10 and 11 VMs, not on a development machine.
- Publishing anything (a release, the manifest) needs the maintainer's explicit
  yes, every time.

## Licence

The project is **GPL-3.0-or-later**. Copyleft is compatible with the
redistributables, for two reasons:

- The client never contains or links the Microsoft installers. It downloads
  them and runs them as separate programs.
- The static MSVC CRT inside `redist.exe` is a "System Library" under GPLv3,
  because it comes with the compiler. GPLv2's equivalent exception does not
  cover a statically linked runtime, so keep the licence on v3 or later.

AGPL's network clause would do nothing for a desktop installer.

Dependencies must be GPLv3-compatible. MIT, Apache-2.0, BSD, ISC, Zlib,
MPL-2.0 and Unicode-3.0 are fine.

## Open questions

The open questions are the maintainer's to decide. Do not settle them in code.
They are the product name, which out-of-support .NET majors to include, and
whether to publish the offline pack.
