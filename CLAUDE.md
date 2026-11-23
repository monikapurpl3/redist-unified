# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

**If `PRIVATE.md` exists, read it first.** It is local only and never committed.
It holds the hosting and publishing setup, where the signing key is kept, and
details of the maintainer's machine. `redist-installer-plan.md` (the full design
plan) and `ASPIC.md` are local-only files as well. This file carries the public
part of the design.

## State of the project

Milestone 1 is the manifest and detection library. The manifest schema and its
parser are done. Detection (evaluating the rules against a real or fake
registry and file system) is next.

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
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --check
```

Keep the build free of warnings. Cargo warns about unused dependencies,
including unused `[workspace.dependencies]` entries, so add a dependency in the
same change as the code that uses it.

**Check the static CRT after changing the build setup.** Run
`dumpbin -nologo -dependents target/release/redist.exe`. The client must not
import `VCRUNTIME140.dll` or any `api-ms-win-crt-*` DLL, because installing
those is its job. Only `KERNEL32.dll`, `ntdll.dll` and `api-ms-win-core-*` API
sets are expected. A build without `+crt-static` lists the CRT DLLs.

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

- `crates/redist-core` is the library: manifest, detection and install logic,
  with unit tests against fake registries. Milestone 1 is this library, the real
  product.
- `crates/redist` is the thin binary (CLI, later the UI) that builds
  `redist.exe`. It does not depend on `redist-core` yet; add the path
  dependency when it first calls the library.
- The repository is called `redist-unified`. The crate and exe names are
  working names and have not been renamed to match.
- `.cargo/config.toml` sets `+crt-static` for `x86_64-pc-windows-msvc` (the
  first target) and `aarch64-pc-windows-msvc` (later).
- The `requireAdministrator` manifest is not embedded yet. It comes with the CLI
  installer in milestone 2.
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
    is refused. This works on 32-bit Windows too.
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
    `url` and `sha256`.
- **Arch:** `x86`, `x64` or `arm64`, plus `any` for components that are not tied
  to one architecture (DirectX, Windows features).
- **Exit codes:** `exit_ok`, `exit_reboot`, `exit_already` and `exit_retry`
  are all required, can be empty except `exit_ok`, and never share a code. A
  code in none of them means failure.
- **`latest_client`** is `version` and `url` only. The client never downloads
  itself, so it has no hash.
- **What `parse` leaves out:** it does not verify the signature and does not
  decide acceptance (replay protection, `min_client`). Both belong to
  milestone 3.
- **The fixture is not real data:** its URLs and hashes are dummies, and its
  silent arguments and detection values have not been checked against the real
  installers.

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
They are the product name, which out-of-support .NET majors to include, console
or UI for v1, and whether to publish the offline pack.
