# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

**If `PRIVATE.md` exists, read it first.** It is local only and never committed.
It holds the hosting and publishing setup, where the signing key is kept, and
details of the maintainer's machine. `redist-installer-plan.md` (the full design
plan) and `ASPIC.md` are local-only files as well. This file carries the public
part of the design.

## State of the project

Milestone 1 (the manifest and detection library) has started. So far only the
Cargo workspace exists, and it has no logic yet.

## Commands

The project builds with **nightly**, which is the maintainer's choice and is
selected by `rust-toolchain.toml`. Quote the path in shell commands: the folder
name contains a space.

```bash
cargo build                                          # debug build
cargo build --release --locked                       # target/release/redist.exe
cargo test --locked                                  # every crate
cargo test -p redist-core <name>                     # tests whose name contains <name>
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

The open questions are the maintainer's to decide. Do not settle them in code.
They are the product name, which out-of-support .NET majors to
include, console or UI for v1, whether to publish the offline pack, and the
licence (MIT or Apache-2.0). Because the licence is undecided, the crates have
no `license` field yet.
