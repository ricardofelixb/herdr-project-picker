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
    let known = ["open", "pick", "on-pane-created"];
    for table in ["actions", "panes", "events"] {
        for item in items(&m, table) {
            let argv = command(item);
            assert_eq!(argv[0], binary);
            assert!(known.contains(&argv[1]), "{argv:?}");
        }
    }
    let build = command(&items(&m, "build")[0]);
    assert_eq!(&build[..3], ["cargo", "build", "--release"]);
}

#[test]
fn the_picker_is_a_popup_and_the_only_hook_is_pane_created() {
    let m = manifest();
    let panes = items(&m, "panes");
    assert_eq!(panes[0]["id"].as_str(), Some("picker"));
    assert_eq!(panes[0]["placement"].as_str(), Some("popup"));
    let events = items(&m, "events");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["on"].as_str(), Some("pane.created"));
}
