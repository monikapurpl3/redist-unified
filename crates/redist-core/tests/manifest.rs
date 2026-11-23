//! Manifest parsing, against `data/manifest.json` and edited copies of it.

mod common;

use common::{DIRECTX, DOTNET, NETFX, VC_X64, VC_X86, component, fixture, parse_edited, remove};
use redist_core::manifest::{
    Arch, Detect, DotnetFramework, ExitOutcome, Installer, KnownFolder, Manifest, ManifestError,
    RegistryCheck, RegistryData, RegistryRule, RegistryView,
};
use redist_core::version::Version;
use serde_json::{Value, json};

/// Asserts that the fixture is refused after `edit`, with `expected` in the
/// message.
#[track_caller]
fn assert_refused(edit: impl FnOnce(&mut Value), expected: &str) {
    match parse_edited(edit) {
        Ok(_) => panic!("the edited manifest was accepted; expected {expected:?}"),
        Err(error) => {
            let message = error.to_string();
            assert!(
                message.contains(expected),
                "expected {expected:?} in {message:?}"
            );
        }
    }
}

/// The x64 VC++ rule that checks `Bld`.
fn bld_rule(manifest: &mut Value) -> &mut Value {
    &mut manifest["components"][VC_X64]["detect"]["rules"][1]
}

fn parsed_bld_rule(manifest: &Manifest) -> &RegistryRule {
    let Detect::All { rules } = &manifest.components[VC_X64].detect else {
        panic!("expected an all rule");
    };
    let Detect::Registry(rule) = &rules.as_slice()[1] else {
        panic!("expected a registry rule");
    };
    rule
}

#[test]
fn the_fixture_parses() {
    let manifest = fixture();
    assert_eq!(manifest.version, 42);
    assert_eq!(manifest.issued.to_string(), "2026-11-23T12:00:00Z");
    assert_eq!(manifest.min_client, "0.1".parse::<Version>().unwrap());
    assert_eq!(manifest.components.len(), 5);

    let vc = manifest.component("vcredist-2015-2022-x64").unwrap();
    assert_eq!(vc.arch, Arch::X64);
    assert_eq!(vc.version.to_string(), "14.44.35211.0");
    let download = vc.download.as_ref().unwrap();
    assert_eq!(
        download.url.as_str(),
        "https://download.example/vc_redist.x64.exe"
    );
    assert_eq!(download.sha256.as_bytes()[31], 1);
    assert_eq!(
        vc.installer,
        Installer::Exe {
            args: vec!["/install".into(), "/quiet".into(), "/norestart".into()]
        }
    );
    let bld = parsed_bld_rule(&manifest);
    assert_eq!(
        bld.key.subkey(),
        r"SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64"
    );
    assert_eq!(bld.view, RegistryView::Bits64);
    assert_eq!(bld.value.as_deref(), Some("Bld"));
    assert_eq!(bld.check, RegistryCheck::Min(35211));

    let Detect::All { rules } = &manifest.components[VC_X86].detect else {
        panic!("expected an all rule");
    };
    let Detect::Registry(installed) = &rules.as_slice()[0] else {
        panic!("expected a registry rule");
    };
    assert_eq!(installed.view, RegistryView::Bits32);
    assert_eq!(
        installed.check,
        RegistryCheck::Equals(RegistryData::Number(1))
    );

    let directx = &manifest.components[DIRECTX];
    let Installer::Extract {
        extract_args, run, ..
    } = &directx.installer
    else {
        panic!("expected an extract installer");
    };
    assert_eq!(extract_args, &["/Q", "/T:{dir}"]);
    assert_eq!(run.as_str(), "DXSETUP.exe");
    let Detect::All { rules } = &directx.detect else {
        panic!("expected an all rule");
    };
    let Detect::File(file) = &rules.as_slice()[1] else {
        panic!("expected a file rule");
    };
    assert_eq!(file.path.folder(), KnownFolder::SysWow64);
    assert_eq!(file.path.relative().as_str(), "d3dx9_43.dll");
    assert_eq!(file.min_version, None);

    let Detect::DotnetRuntime(dotnet) = &manifest.components[DOTNET].detect else {
        panic!("expected a dotnet_runtime rule");
    };
    assert_eq!(dotnet.framework, DotnetFramework::WindowsDesktop);
    assert_eq!(
        dotnet.framework.folder_name(),
        "Microsoft.WindowsDesktop.App"
    );
    assert_eq!(dotnet.min.major(), 8);

    let netfx = &manifest.components[NETFX];
    assert_eq!(netfx.arch, Arch::Any);
    assert_eq!(netfx.download, None);
    assert!(!netfx.installer.downloads());
}

#[test]
fn exit_codes_are_classified_per_component() {
    let manifest = fixture();
    let vc = &manifest.components[VC_X64].exit_codes;
    assert_eq!(vc.classify(0), ExitOutcome::Installed);
    assert_eq!(vc.classify(3010), ExitOutcome::RebootNeeded);
    assert_eq!(vc.classify(1641), ExitOutcome::RebootNeeded);
    assert_eq!(vc.classify(1638), ExitOutcome::AlreadyInstalled);
    assert_eq!(vc.classify(1618), ExitOutcome::RetryLater);
    assert_eq!(vc.classify(1603), ExitOutcome::Failed);

    // DirectX lists no reboot code, so 3010 is a failure there.
    let directx = &manifest.components[DIRECTX].exit_codes;
    assert_eq!(directx.classify(3010), ExitOutcome::Failed);
}

#[test]
fn a_newer_schema_is_reported_before_anything_else() {
    let error = Manifest::parse(br#"{"schema": 2, "anything": "else"}"#).unwrap_err();
    assert!(matches!(error, ManifestError::UnsupportedSchema(2)));
    assert!(error.to_string().contains("only understands schema 1"));
}

#[test]
fn garbage_is_a_json_error() {
    assert!(matches!(
        Manifest::parse(b"not json"),
        Err(ManifestError::Json(_))
    ));
    assert!(matches!(
        Manifest::parse(br#"{"version": 42}"#),
        Err(ManifestError::Json(_))
    ));
}

#[test]
fn unknown_fields_are_refused_at_every_level() {
    assert_refused(
        |m| m["signed_by"] = json!("someone"),
        "unknown field `signed_by`",
    );
    assert_refused(
        |m| m["latest_client"]["sha256"] = json!("00"),
        "unknown field `sha256`",
    );
    assert_refused(
        |m| component(m, VC_X64)["exit_rebot"] = json!([3010]),
        "unknown field `exit_rebot`",
    );
    assert_refused(
        |m| component(m, VC_X64)["installer"]["silent"] = json!(true),
        "unknown field `silent`",
    );
    assert_refused(
        |m| component(m, VC_X64)["detect"]["mode"] = json!("strict"),
        "unknown field `mode`",
    );
    assert_refused(
        |m| bld_rule(m)["hive"] = json!("HKLM"),
        "unknown field `hive`",
    );
    assert_refused(
        |m| component(m, DOTNET)["detect"]["arch"] = json!("x64"),
        "unknown field `arch`",
    );
    assert_refused(
        |m| component(m, DIRECTX)["detect"]["rules"][0]["exists"] = json!(true),
        "unknown field `exists`",
    );
}

#[test]
fn top_level_values_are_checked() {
    assert_refused(|m| m["version"] = json!(0), "version must be at least 1");
    assert_refused(|m| m["components"] = json!([]), "there are no components");
    assert_refused(
        |m| m["issued"] = json!("2026-02-30T12:00:00Z"),
        "not a real time",
    );
    assert_refused(
        |m| m["issued"] = json!("2026-11-23 12:00:00"),
        "YYYY-MM-DDTHH:MM:SSZ",
    );
    assert_refused(
        |m| m["min_client"] = json!("0.2.0-beta"),
        "not dot-separated numbers",
    );
    assert_refused(
        |m| m["latest_client"]["url"] = json!("ftp://download.example/"),
        "is not https://",
    );
}

#[test]
fn component_ids_are_checked_and_unique() {
    assert_refused(
        |m| component(m, VC_X86)["id"] = json!("vcredist-2015-2022-x64"),
        "\"vcredist-2015-2022-x64\" is used twice",
    );
    assert_refused(
        |m| component(m, VC_X64)["id"] = json!("VC_Redist"),
        "lowercase letters, digits and -",
    );
    assert_refused(
        |m| component(m, VC_X64)["id"] = json!("-vc"),
        "not starting or ending with -",
    );
    assert_refused(
        |m| component(m, VC_X64)["name"] = json!("  "),
        "the name is empty",
    );
    assert_refused(
        |m| component(m, VC_X64)["arch"] = json!("ia64"),
        "unknown variant `ia64`",
    );
}

#[test]
fn downloads_are_https_and_hashed() {
    assert_refused(
        |m| component(m, VC_X64)["url"] = json!("http://download.example/vc_redist.x64.exe"),
        "is not https://",
    );
    assert_refused(
        |m| component(m, VC_X64)["sha256"] = json!("abc"),
        "is not 64 hex digits",
    );
    assert_refused(
        |m| component(m, VC_X64)["sha256"] = json!("g".repeat(64)),
        "is not 64 hex digits",
    );
    assert_refused(
        |m| remove(component(m, VC_X64), "sha256"),
        "its installer needs url and sha256",
    );
    assert_refused(
        |m| remove(component(m, DIRECTX), "url"),
        "its installer needs url and sha256",
    );
    assert_refused(
        |m| component(m, NETFX)["url"] = json!("https://download.example/netfx35.exe"),
        "a windows_feature installer downloads nothing",
    );
}

#[test]
fn exit_codes_never_overlap() {
    assert_refused(
        |m| component(m, VC_X64)["exit_retry"] = json!([1618, 3010]),
        "exit code 3010 is listed more than once",
    );
    assert_refused(
        |m| component(m, VC_X64)["exit_ok"] = json!([0, 0]),
        "exit code 0 is listed more than once",
    );
    assert_refused(
        |m| component(m, VC_X64)["exit_ok"] = json!([]),
        "exit_ok needs at least one code",
    );
    assert_refused(
        |m| component(m, VC_X64)["exit_ok"] = json!([-1]),
        "invalid value",
    );
    assert_refused(
        |m| remove(component(m, DIRECTX), "exit_retry"),
        "missing field `exit_retry`",
    );
}

#[test]
fn installers_are_checked() {
    assert_refused(
        |m| component(m, DIRECTX)["installer"]["extract_args"] = json!(["/Q"]),
        "extract_args must contain {dir}",
    );
    assert_refused(
        |m| component(m, DIRECTX)["installer"]["run"] = json!(r"..\DXSETUP.exe"),
        "is not a relative Windows path",
    );
    assert_refused(
        |m| component(m, NETFX)["installer"]["feature"] = json!("NetFx3 /All"),
        "feature name \"NetFx3 /All\"",
    );
    assert_refused(
        |m| component(m, VC_X64)["installer"] = json!({"type": "script", "args": []}),
        "unknown variant `script`",
    );

    let manifest = parse_edited(|m| {
        component(m, VC_X64)["installer"] = json!({"type": "msi", "args": ["/qn", "/norestart"]});
    })
    .unwrap();
    assert!(matches!(
        manifest.components[VC_X64].installer,
        Installer::Msi { .. }
    ));
}

#[test]
fn registry_rules_take_one_comparison_on_a_named_value() {
    assert_refused(|m| bld_rule(m)["equals"] = json!(1), "at most one of");
    assert_refused(|m| remove(bld_rule(m), "value"), "needs a value name");
    assert_refused(
        |m| bld_rule(m)["view"] = json!("wow64"),
        "unknown variant `wow64`",
    );

    // Without a comparison, a rule checks that the value exists.
    let manifest = parse_edited(|m| remove(bld_rule(m), "min")).unwrap();
    assert_eq!(parsed_bld_rule(&manifest).check, RegistryCheck::Exists);

    // Without a value either, it checks that the key exists.
    let manifest = parse_edited(|m| {
        remove(bld_rule(m), "min");
        remove(bld_rule(m), "value");
    })
    .unwrap();
    let rule = parsed_bld_rule(&manifest);
    assert_eq!(
        (rule.value.as_deref(), &rule.check),
        (None, &RegistryCheck::Exists)
    );

    let manifest = parse_edited(|m| {
        remove(bld_rule(m), "min");
        bld_rule(m)["equals"] = json!("14.44");
    })
    .unwrap();
    assert_eq!(
        parsed_bld_rule(&manifest).check,
        RegistryCheck::Equals(RegistryData::String("14.44".into()))
    );

    let manifest = parse_edited(|m| {
        remove(bld_rule(m), "min");
        bld_rule(m)["min_version"] = json!("14.44.35211");
    })
    .unwrap();
    assert_eq!(
        parsed_bld_rule(&manifest).check,
        RegistryCheck::MinVersion("14.44.35211".parse().unwrap())
    );
}

#[test]
fn registry_keys_are_machine_wide_and_without_wow6432node() {
    assert_refused(
        |m| {
            bld_rule(m)["key"] =
                json!(r"HKLM\SOFTWARE\WOW6432Node\Microsoft\VisualStudio\14.0\VC\Runtimes\x86");
        },
        "names WOW6432Node; drop it and set \"view\": \"32\"",
    );
    assert_refused(
        |m| bld_rule(m)["key"] = json!(r"HKCU\Software\Example"),
        "is not under HKLM",
    );
    assert_refused(
        |m| bld_rule(m)["key"] = json!(r"HKLM\SOFTWARE\\Example"),
        "has an empty part",
    );

    let manifest = parse_edited(|m| {
        bld_rule(m)["key"] = json!(r"HKEY_LOCAL_MACHINE\SOFTWARE\Example");
    })
    .unwrap();
    assert_eq!(parsed_bld_rule(&manifest).key.subkey(), r"SOFTWARE\Example");
}

#[test]
fn file_paths_start_at_a_known_folder() {
    fn first_file(m: &mut Value) -> &mut Value {
        &mut m["components"][DIRECTX]["detect"]["rules"][0]
    }
    assert_refused(
        |m| first_file(m)["path"] = json!(r"C:\Windows\System32\d3dx9_43.dll"),
        "does not start with a known folder",
    );
    assert_refused(
        |m| first_file(m)["path"] = json!(r"{system}\d3dx9_43.dll"),
        "starts with an unknown folder",
    );
    assert_refused(
        |m| first_file(m)["path"] = json!(r"{system32}\..\d3dx9_43.dll"),
        "is not a relative Windows path",
    );
    assert_refused(
        |m| first_file(m)["path"] = json!("{system32}/d3dx9_43.dll"),
        "needs a \\ and a path after {system32}",
    );
    assert_refused(
        |m| first_file(m)["path"] = json!("{system32}"),
        "needs a \\ and a path after {system32}",
    );

    let manifest = parse_edited(|m| first_file(m)["min_version"] = json!("9.29.952.3111")).unwrap();
    let Detect::All { rules } = &manifest.components[DIRECTX].detect else {
        panic!("expected an all rule");
    };
    let Detect::File(file) = &rules.as_slice()[0] else {
        panic!("expected a file rule");
    };
    assert_eq!(file.min_version, Some("9.29.952.3111".parse().unwrap()));
}

#[test]
fn other_detection_rules() {
    assert_refused(
        |m| component(m, VC_X64)["detect"] = json!({"type": "any", "rules": []}),
        "`all` and `any` need at least one rule",
    );
    assert_refused(
        |m| component(m, VC_X64)["detect"] = json!({"type": "wmi"}),
        "unknown variant `wmi`",
    );
    assert_refused(
        |m| component(m, DOTNET)["detect"]["framework"] = json!("Microsoft.Maui.App"),
        "unknown variant `Microsoft.Maui.App`",
    );
    assert_refused(
        |m| component(m, DOTNET)["arch"] = json!("any"),
        "a dotnet_runtime rule needs the component's arch",
    );
    assert_refused(
        |m| {
            component(m, NETFX)["detect"] = json!({"type": "all", "rules": [
                {"type": "dotnet_runtime", "framework": "Microsoft.NETCore.App", "min": "8.0.0"}
            ]});
        },
        "a dotnet_runtime rule needs the component's arch",
    );
    assert_refused(
        |m| component(m, VC_X64)["detect"]["rules"][1]["value"] = json!("Bld\u{0}"),
        "a registry value name cannot have control characters",
    );

    let command = json!({
        "type": "command",
        "program": r"{system32}\example.exe",
        "args": ["/check"],
        "exit_installed": [0]
    });
    let manifest = parse_edited(|m| component(m, NETFX)["detect"] = command.clone()).unwrap();
    let Detect::Command(rule) = &manifest.components[NETFX].detect else {
        panic!("expected a command rule");
    };
    assert_eq!(rule.program.folder(), KnownFolder::System32);
    assert_eq!(rule.exit_installed, [0]);

    assert_refused(
        |m| {
            component(m, NETFX)["detect"] = command;
            component(m, NETFX)["detect"]["exit_installed"] = json!([]);
        },
        "needs at least one code in exit_installed",
    );

    // Rules nest.
    parse_edited(|m| {
        component(m, NETFX)["detect"] = json!({
            "type": "any",
            "rules": [
                {"type": "all", "rules": [{"type": "file", "path": r"{windows}\a.dll"}]},
                {"type": "file", "path": r"{program_files_x86}\b\c.dll"}
            ]
        });
    })
    .unwrap();
}
