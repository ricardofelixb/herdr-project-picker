//! Guards the contract between `herdr-plugin.toml` and the binary: Herdr never
//! runs a shell, so a typo in a command would only surface at runtime.

use toml::Value;

fn manifest() -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/herdr-plugin.toml");
    std::fs::read_to_string(path).unwrap().parse().unwrap()
}

fn items<'a>(manifest: &'a Value, table: &str) -> &'a [Value] {
    manifest[table].as_array().unwrap()
}

fn command(item: &Value) -> Vec<&str> {
    item["command"]
        .as_array()
        .unwrap()
        .iter()
        .map(|part| part.as_str().unwrap())
        .collect()
}

fn valid_id(id: &str, allow_dot: bool) -> bool {
    !id.is_empty()
        && id.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '-') || (allow_dot && c == '.')
        })
}

#[test]
fn required_fields_and_ids_follow_the_plugin_rules() {
    let m = manifest();
    for key in ["id", "name", "version", "min_herdr_version"] {
        assert!(m[key].as_str().is_some(), "missing {key}");
    }
    assert!(valid_id(m["id"].as_str().unwrap(), true));
    for table in ["actions", "panes"] {
        for item in items(&m, table) {
            assert!(valid_id(item["id"].as_str().unwrap(), false), "{table} id");
        }
    }
}

#[test]
fn commands_run_the_built_binary_with_known_subcommands() {
    let m = manifest();
    let binary = format!("target/release/{}", env!("CARGO_PKG_NAME"));
    let known = ["open", "pick"];
    for table in ["actions", "panes"] {
        for item in items(&m, table) {
            let argv = command(item);
            assert_eq!(argv[0], binary);
            assert!(known.contains(&argv[1]), "{argv:?}");
        }
    }
    let build = command(&items(&m, "build")[0]);
    assert_eq!(&build[..3], ["cargo", "build", "--release"]);
}

// Manual-only: Herdr cannot tell a user split from a new tab's first pane in
// `pane.created`, so no event hook may launch the picker on its own.
#[test]
fn the_picker_is_a_popup_and_nothing_launches_it_automatically() {
    let m = manifest();
    let panes = items(&m, "panes");
    assert_eq!(panes[0]["id"].as_str(), Some("picker"));
    assert_eq!(panes[0]["placement"].as_str(), Some("popup"));
    assert!(m.get("events").is_none(), "event hooks are removed");
}

fn all_commands(m: &Value) -> Vec<Vec<&str>> {
    ["actions", "panes"]
        .iter()
        .flat_map(|table| items(m, table))
        .map(command)
        .collect()
}

// Herdr runs argv directly, never through a shell, so the same manifest must
// work on Windows, macOS and Linux as written.
#[test]
fn commands_are_shell_free_and_portable_across_platforms() {
    let m = manifest();
    let platforms: Vec<&str> = m["platforms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap())
        .collect();
    for os in ["linux", "macos", "windows"] {
        assert!(platforms.contains(&os), "{os}");
    }
    for argv in all_commands(&m) {
        let program = std::path::Path::new(argv[0]);
        assert!(!argv[0].contains('\\'), "forward slashes only: {argv:?}");
        assert!(
            program.is_relative(),
            "relative to the plugin folder: {argv:?}"
        );
        assert!(
            program.extension().is_none(),
            "Windows adds .exe itself: {argv:?}"
        );
        assert!(
            !["sh", "bash", "cmd", "powershell", "pwsh"].contains(&argv[0]),
            "{argv:?}"
        );
    }
}

// Herdr's Windows pane launcher finds `<program>.exe` by swapping the program's
// extension, which would clip a dotted binary name.
#[test]
fn the_binary_name_survives_windows_extension_lookup() {
    assert!(!env!("CARGO_PKG_NAME").contains('.'));
}

#[test]
fn the_build_puts_the_binary_where_the_commands_look() {
    let m = manifest();
    let build = command(&items(&m, "build")[0]);
    assert_eq!(build[0], "cargo");
    for flag in ["--release", "--locked"] {
        assert!(build.contains(&flag), "{flag}");
    }
    let at = build.iter().position(|a| *a == "--target-dir").unwrap();
    assert_eq!(build[at + 1], "target");
    for argv in all_commands(&m) {
        assert_eq!(
            argv[0],
            format!("target/release/{}", env!("CARGO_PKG_NAME"))
        );
    }
}

// The key binding the README and `bind-key` use comes from these ids.
#[test]
fn the_action_users_bind_exists_in_the_manifest() {
    let m = manifest();
    let action = &items(&m, "actions")[0];
    assert_eq!(action["id"].as_str(), Some("pick"));
    assert_eq!(m["id"].as_str(), Some("ricardofelixb.project-picker"));
    let contexts = action["contexts"].as_array().unwrap();
    assert!(contexts.iter().any(|c| c.as_str() == Some("pane")));
}
