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
2. The CLI installer. Done. It is tested against fakes, and it really installs
   from a real manifest (below) on throwaway x64 and ARM64 runners, and in a
   throwaway container, where every component in it was missing. Not yet seen
   for real: an installer that asks for a restart.
3. The UI. In progress. The Slint window is built (see "The window" below). It
   is tested through its logic, through screenshots drawn offscreen, and
   through a safe demo. `redist.exe` has run for real once, downloaded from a
   web page into Windows Sandbox (see "Local real installs").
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
cargo build --release --locked                       # target/release/redist.exe and redist-cli.exe
cargo build --release --locked --target aarch64-pc-windows-msvc   # the same for ARM64 (see below)
cargo test --locked                                  # every crate
cargo test -p redist-core <name>                     # tests whose name contains <name>
cargo test -p redist-core --test manifest            # one integration test file
cargo test -p redist-core --lib windows              # the real Windows backend (read-only)
cargo test -p redist-core --test download            # the download engine, against a fake server
cargo test -p redist-core --lib -- --ignored         # real downloads from this repo on GitHub
cargo run -p redist-core --example detect -- e2e/manifest.json   # what its rules find here (read-only)
cargo run -p redist --example demo [-- --fail|--busy|--nothing] # the real window on a pretend machine
cargo run -p redist --example demo -- --headless     # the same, in the console
cargo run -p redist --example screenshots -- <folder> # every window state as PNG, offscreen
cargo run -p redist --example icon [-- <folder>]     # assets/redist.ico from the SVGs (and PNGs)
pwsh -File e2e/container-test.ps1                    # real installs in a throwaway container (elevated)
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --check
```

**Never start `redist.exe` or `redist-cli.exe` on the development machine.**
Both require administrator rights and install runtimes, so test them in a
throwaway Windows (see the Invariants). The `demo` and `screenshots` examples
are the safe way to see the window here.
- From an unelevated shell, `cargo run` fails with "requires elevation"
  (os error 740).
- From PowerShell, even `redist.exe --help` raises a UAC prompt, because
  PowerShell hands programs that need elevation to the Windows shell.
- **From an elevated shell nothing stops them.** `cargo run -p redist`, or
  starting either exe, installs runtimes on the development machine at once,
  with no prompt. So never `cargo run` the exes, elevated or not. The
  examples, the tests and `e2e/container-test.ps1` stay safe when elevated.
- To check the exe without running it, read its manifest with `mt.exe`
  (Windows SDK):

  ```bash
  mt.exe -nologo -inputresource:target\release\redist.exe;#1 -out:redist.manifest
  ```

**Building for ARM64 needs Visual Studio's ARM64 build tools**, the
component "MSVC Build Tools for ARM64/ARM64EC"
(`Microsoft.VisualStudio.Component.VC.Tools.ARM64`), in the toolset Rust
uses. Without it Rust finds no ARM64 `link.exe` and falls back to whatever
`link` is on PATH (Git Bash's fails with "extra operand"). The x64 libraries'
`lib\arm64` folder alone is not enough: it lacks the C runtime.

Keep the build free of warnings. Cargo warns about unused dependencies,
including unused `[workspace.dependencies]` entries, so add a dependency in the
same change as the code that uses it.

**Check the static CRT after changing the build setup.** Run
`dumpbin -nologo -dependents` on both exes. They must not import
`VCRUNTIME140.dll` or any `api-ms-win-crt-*` DLL, because installing those is
their job. Only DLLs that are part of Windows are expected:
- in both: `kernel32`, `ntdll`, `advapi32`, `shell32`, `combase`, `version`,
  `winhttp`, `bcryptprimitives` and the `api-ms-win-core-*` API sets;
- in the window also: `user32`, `gdi32`, `dwmapi`, `uxtheme`, `imm32`,
  `comctl32`, `dwrite`, `uiautomationcore`, `ole32`, `oleaut32` and `shlwapi`.

A build without `+crt-static` lists the CRT DLLs. CI checks all of this.

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
- `crates/redist` builds the two exes. Its library holds everything with
  tests.
  - **`redist.exe` is the window**, what users double-click (`src/main.rs`,
    a GUI-subsystem program). See "The window" below. It takes
    `--manifest <file>`, or else `manifest.json` next to the exe; both are
    unsigned, for development only.
  - **`redist-cli.exe` is the console version**, for CI and scripts
    (`src/bin/redist-cli.rs`).
    - For now `--manifest <file>` reads an unsigned local manifest, for
      development only. `--only <id>` installs one component, and `--check`
      only detects.
    - Exit codes: 0, 1 (failure), 2 (usage or manifest error), and 3010 (a
      restart is still needed).
    - Its command line and every line of text it prints are in `src/lib.rs`.
  - **`src/context.rs`** is what both set up before a run. It holds the
    machine, downloader, runner and restart as trait objects, so the demo can
    swap in stand-ins.
- The repository is called `redist-unified`. The crate and exe names are
  working names and have not been renamed to match.
- `.cargo/config.toml` sets `+crt-static` for `x86_64-pc-windows-msvc` (the
  first target) and `aarch64-pc-windows-msvc` (later).
- **Application manifests.** `crates/redist/build.rs` embeds
  `requireAdministrator` plus `crates/redist-core/windows-compat.manifest` (the
  Windows 10/11 `supportedOS` GUID) through `rustc-link-arg-bins`.
  - That link argument also reaches the exes' own test builds, which then
    cannot run unelevated. So both `[[bin]]`s have `test = false`, and their
    tested code lives in the crate's library. Examples are not reached: they
    run unelevated.
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
  - Both exes delete its subfolders at the end, and keep the log and the
    installers' output.

### The window

The window is built with **Slint 1.19** (the maintainer's choice). The brief:
cozy and modern, yet responsive and minimalistic. Its text is English for now;
every line is in `src/ui.rs`, so translations can come later.
- **Files:**
  - `ui/app.slint` draws it, compiled by `build.rs`.
  - `src/ui.rs` (`UiModel`) decides everything it shows, and has the tests:
    rows, phase, headline, sentence, progress.
  - `src/gui.rs` copies a `UiModel` into the window.
  - `src/app.rs` runs it.
- **The flow:**
  1. It checks right away.
  2. It shows what is missing, with one **Install** button and **Not now**.
  3. Then it shows progress.
  4. Then **All done**, or **Restart now / Later**, or **Something went
     wrong** with **Open log**.
- **The icon** is the maintainer's design: a soft white cross and a green
  one, on a rounded black square.
  - `assets/icon.svg` is the drawing, and `assets/icon-16.svg` its redraw on
    the 16 px grid. `cargo run -p redist --example icon` renders both with
    `resvg` (Slint's own SVG renderer) into `assets/redist.ico`: 16 px from
    the small drawing, 20 to 256 px from the main one. It reproduces byte
    for byte, and `*.ico` is binary in `.gitattributes`.
  - `build.rs` turns the `.ico` into a compiled resource (`.res`) itself, so
    no resource compiler is needed; `link.exe` takes `.res` files as inputs.
    It is icon resource 1 in both exes and in the examples, so the demo has
    it too.
  - `app.rs` loads it into the title bar (16 logical px) and the taskbar
    (32) at the window's scale, through winit (Slint's `unstable-winit-030`
    feature, tied to the Slint version). Slint's own `icon` property would
    give both one 64 px picture. Seen working: at 150 % the demo window
    carries 24 and 48 px icons, and Windows extracts it as both exes' icon.
- **Responsive.** Worker threads run the session and only update a shared
  `UiModel`. A timer copies it into the window every 80 ms, and only when it
  changed. Closing the window is refused while an install runs.
- **Rendering.** Slint draws on the CPU (`renderer-software`), so the window
  works on old PCs, in remote sessions and in Sandbox, where GPU drawing can
  fail. The style is `fluent`, which follows Windows' light or dark mode.
- **Size.** `redist.exe` is about 12.4 MB; `redist-cli.exe` is 0.8 MB.
  - Most of the size is Slint's winit backend, which switches on image and
    SVG decoding itself.
  - Optimising for size (`opt-level` "s" or "z") only gives 10.4 or 9.6 MB, at
    the cost of drawing speed, so it is not used.
- **Gotchas:**
  - `row` is a built-in layout property and `close` a built-in window
    function, so the code uses `item` and `dismiss`.
  - The header and the button row have `vertical-stretch: 0`. Without it, an
    empty button row (while working) grows and squashes the list.
  - Fluent buttons keep some colours when the colour scheme switches in a
    running window. The real window does not switch, so the screenshot tool
    makes a fresh window for every picture.
- **Seeing it without elevation:**
  - The `screenshots` example draws every state, light and dark, into PNG
    files, offscreen. CI uploads them as `screenshots`.
  - The `demo` example runs the real window and the real session on a pretend
    machine: generated downloads with real hashes, throttled so the progress
    shows, installers that take two and a half seconds, and a restart that
    only closes the window.
    - It touches no registry, system file or network, and needs no elevation.
    - Its scenarios are `--fail`, `--busy` and `--nothing`; `--headless`
      runs it in the console, as CI does.

## Testing for real

### CI

`.github/workflows/ci.yml` runs on every push to `main` and on pull requests.
- **Build and test**, on `windows-2025`:
  - fmt, clippy, every test, and the network tests;
  - release builds for x64 and ARM64;
  - a check that neither exe imports the VC++ runtime, that both ask for
    administrator rights, and that `redist.exe` is a GUI program while
    `redist-cli.exe` is a console one;
  - the window's screenshots, and the demo run headless.
- **Install**, on `windows-2025` (x64) and `windows-11-arm` (ARM64), with the
  console exe (the window would wait for a click):
  1. `redist-cli.exe --check`
  2. `redist-cli.exe`
  3. `redist-cli.exe --check`, which must say "Nothing is missing."

  The runners already have some runtimes, so this covers both real installs
  and leaving alone what is there. Exit code 3010 skips the last check, since
  the restart question gets no answer there and nothing restarts. The work
  folders' logs are uploaded as `logs-x64` and `logs-arm64`.
- **Actions** are pinned to commit SHAs, with the version in a comment.
  Checkouts keep no credentials, and the workflow can only read.
- **The first green run** (2026-11-24, `0274a21`) behaved the same on both
  runners:
  - VC++ v14 and the .NET 11 Desktop Runtime were already there, and were left
    alone.
  - VC++ 2013 (x64, x86) and DirectX June 2010 were missing. They downloaded,
    installed with exit code 0, and passed detection right after.
  - The last check then said "Nothing is missing."

  On ARM64 Windows 11, that includes the x64 and x86 packages and DirectX's
  `System32` and `SysWOW64` file rules.
- **Parts run in parallel,** so the order of requests in the download tests
  varies. Assert on sorted ranges, or on the order requests really came in,
  never on a sorted list's last element.

### Local real installs

- **`e2e/container-test.ps1`** runs `redist-cli.exe` with `e2e/manifest.json`
  in a throwaway Windows container: check, install, check.
  - It uses containerd and `nerdctl` with Hyper-V isolation, so the image's
    Windows build need not match the host's. It needs an elevated PowerShell.
  - Nothing is installed on the host. The output and the work folders' logs
    land in `target\container-test\out`.
  - The default image, `windows/server:ltsc2025`, has the full Windows API,
    DirectX included. Server Core is about half the size but lacks parts of
    DirectX. Nano Server has no WoW64 and no MSI.
  - A container has no desktop, so it cannot show the window.
  - **Networking.** The script makes a NAT network of nerdctl's own,
    `redist-test`, with its config in its own folder
    (`%ProgramData%\redist-unified-test\cni\conf`). containerd's install
    script leaves a single-plugin `0-containerd-nat.conf` in the default
    folder, and nerdctl 2.4.2 crashes on it (index out of range in
    `verifyNetworkTypes` and in `network create`).
  - **The first run** (2026-11-25), Windows Server 2025 (build 26100) under
    Hyper-V isolation: all seven x64 and x86 components were missing. All seven
    downloaded in 4 seconds, installed with exit code 0 and passed detection
    right after, and the last check said "Nothing is missing." That was the
    first real install of VC++ v14 and .NET 11, which the runners already have.
- **The window, downloaded like a user would** (2026-11-25, Windows
  Sandbox): `redist.exe` and `manifest.json` were
  downloaded with Edge from an unlisted test URL, and `redist.exe` started
  with a double-click.
  - Edge's own SmartScreen check stopped the download: "Make sure you trust
    redist.exe before you open it … isn't commonly downloaded … Publisher:
    Unknown", with **Delete** as the main button and keeping the file in its
    menu. Every user of an unsigned build will meet this; signing fixes the
    publisher line, and reputation comes only with time.
  - Windows SmartScreen showed no dialog: the window started a second after
    the double-click. That is not conclusive. Sandbox runs with UAC off
    (`EnableLUA` 0) and Defender's service stopped. And although Edge saved
    both files straight to the Desktop, only `manifest.json` still carried
    its download mark afterwards; the exe had none. That is unexplained.
  - UAC cannot be tested in Sandbox at all, for the same reason.
  - The window found 7 missing, downloaded them in 5 seconds, installed each
    with exit code 0 and showed "All done", about 55 seconds in all.
- **ARM64** runs in an emulated Windows 11 IoT Enterprise LTSC in QEMU, set up
  unattended and reached over SSH. Each run gets a throwaway overlay of the
  base disk. Emulation is slow. Its scripts and details are local
  (`PRIVATE.md`).
  - The new 24H2 Setup stalls under emulation before it touches the disk, so
    the image is applied by hand from WinPE (`diskpart`, `dism`, `bcdboot`),
    with the unattend file as `Windows\Panther\unattend.xml`.
  - The network is `virtio-net` with Red Hat's signed NetKVM driver. Windows
    binds QEMU's `usb-net` to its serial driver, a COM port.

### The e2e manifest

`e2e/manifest.json` is real data:
- **What it holds:**
  - VC++ v14 (x64, x86, ARM64), from `aka.ms/vc14`;
  - VC++ 2013 (x64, x86), from `aka.ms/highdpimfc2013*`;
  - DirectX June 2010;
  - the .NET 11 Desktop Runtime (x64, x86, ARM64), from `releases.json`.
- **Versioned URLs only:** the `aka.ms` links are resolved to Microsoft's
  versioned URLs, because the short links change file when Microsoft
  publishes a new version.
- **Every installer was checked** before going in (2026-11-24):
  - a valid Microsoft or .NET Authenticode signature;
  - for .NET, the SHA-512 from `releases.json`;
  - for VC++ v14, the SHA-256 embedded in its URL.
- **Detection facts it relies on,** seen on a real machine:
  - VC++ v14's x64 key is in the 64-bit registry view (and mirrored in the
    32-bit one); its x86 key is only in the 32-bit view.
  - **VC++ 2013 writes both its x64 and its x86 key only in the 32-bit view.**
- **A test keeps it honest:** `tests/manifest.rs` makes it parse, and makes
  every URL point at a `*.microsoft.com` server.
- **The `detect` example** shows what any manifest's rules find on the
  machine it runs on. It only reads, and needs no elevation.

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
- The installer requires administrator rights and changes the system. Test it
  only in a throwaway Windows, never on a development machine (see "Local real
  installs"):
  - GitHub Actions' Windows runners (x64 and ARM64), discarded after every job;
  - locally, a Windows container first (the maintainer's preference,
    2026-11-25), and Windows Sandbox for checks by hand;
  - for ARM64, one emulated Windows 11 in QEMU, whose base disk no test run
    changes. It is the only VM, because local ARM64 cannot be had otherwise.
    The maintainer wants no other VMs.
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
- **Slint is used under its GPL-3.0-only option** (it is also offered
  royalty-free or commercially). So a built `redist.exe` is effectively
  distributed under GPL-3.0 only. The project's own source stays
  GPL-3.0-or-later.
- On 2026-11-24 every crate built for Windows (327 of them) had a
  GPLv3-compatible licence option. Recheck after adding dependencies.

## Open questions

The open questions are the maintainer's to decide. Do not settle them in code.
They are the product name, which out-of-support .NET majors to include, and
whether to publish the offline pack.
