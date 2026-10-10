//! `bind-key`: adds the picker's key to a Herdr config without touching
//! anything else in it.
//!
//! The file is only ever appended to, after a backup, and only when the key is
//! free. A key that is already used, or a file that is not valid TOML, is
//! reported and left exactly as it was.

use std::path::{Path, PathBuf};
use toml::Table;

pub const DEFAULT_KEY: &str = "prefix+f";

/// The qualified id Herdr binds keys to: the manifest's plugin id and its
/// first action, so the two can never drift apart.
pub fn action_id() -> String {
    let manifest: toml::Value = include_str!("../herdr-plugin.toml")
        .parse()
        .expect("the bundled manifest is valid TOML");
    format!(
        "{}.{}",
        manifest["id"].as_str().unwrap_or_default(),
        manifest["actions"][0]["id"].as_str().unwrap_or_default()
    )
}

/// `existing` plus the binding, or `None` when it is already there.
pub fn with_binding(existing: &str, key: &str, action: &str) -> Result<Option<String>, String> {
    let table: Table = existing
        .parse()
        .map_err(|err| format!("the config is not valid TOML, so it was left alone: {err}"))?;
    if let Some(keys) = table.get("keys").and_then(|keys| keys.as_table()) {
        for (name, value) in keys {
            if name == "command" {
                for entry in value.as_array().into_iter().flatten() {
                    // Every field of a command binding is optional in Herdr.
                    if entry
                        .get("key")
                        .is_some_and(|chords| holds_key(chords, key))
                    {
                        let field = |name| entry.get(name).and_then(|value| value.as_str());
                        let ours = field("type") == Some("plugin_action")
                            && field("command") == Some(action);
                        return if ours {
                            Ok(None)
                        } else {
                            Err(format!(
                                "{key} is already bound to another command; not changed"
                            ))
                        };
                    }
                }
            } else if holds_key(value, key) {
                return Err(format!("{key} is already used by keys.{name}; not changed"));
            }
        }
    }
    let mut text = existing.to_string();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&format!(
        "\n[[keys.command]]\nkey = {key:?}\ntype = \"plugin_action\"\ncommand = {action:?}\n\
         description = \"Open project\"\n"
    ));
    text.parse::<Table>()
        .map_err(|err| format!("the new binding would not be valid TOML ({err}); not changed"))?;
    Ok(Some(text))
}

fn same_key(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// `keys.<action> = "chord"` or `["chord", ...]`.
fn holds_key(value: &toml::Value, key: &str) -> bool {
    match value {
        toml::Value::String(chord) => same_key(chord, key),
        toml::Value::Array(chords) => chords.iter().any(|chord| holds_key(chord, key)),
        _ => false,
    }
}

/// Adds the binding to the file at `config`, keeping a copy of the original
/// beside it. Returns what happened.
pub fn bind(config: &Path, key: &str, action: &str) -> Result<String, String> {
    let existing =
        std::fs::read_to_string(config).map_err(|err| format!("{}: {err}", config.display()))?;
    let Some(updated) = with_binding(&existing, key, action)? else {
        return Ok(format!(
            "{key} is already bound to {action}; nothing changed."
        ));
    };
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let backup = sibling(config, &format!("before-project-picker.{stamp}.bak"));
    std::fs::copy(config, &backup).map_err(|err| format!("{}: {err}", backup.display()))?;
    let staged = sibling(config, "project-picker.tmp");
    std::fs::write(&staged, updated).map_err(|err| format!("{}: {err}", staged.display()))?;
    std::fs::rename(&staged, config).map_err(|err| format!("{}: {err}", config.display()))?;
    Ok(format!(
        "Bound {key} to {action} in {}.\nBackup: {}\nReload the Herdr client config (or restart the client) to use it.",
        config.display(),
        backup.display()
    ))
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(suffix);
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tempdir::TempDir;

    const ACTION: &str = "local.project-picker.pick";

    const USER_CONFIG: &str = r#"onboarding = false

[theme]
name = "kanagawa"

[keys]
edit_scrollback = ""
next_agent = ["ctrl+alt+down", "prefix+x"]

[[keys.command]]
key = "prefix+o"
type = "plugin_action"
command = "someone.else.toggle"
"#;

    #[test]
    fn the_action_id_follows_the_manifest() {
        assert_eq!(action_id(), ACTION);
    }

    #[test]
    fn the_binding_is_appended_and_everything_else_is_preserved() {
        let updated = with_binding(USER_CONFIG, "prefix+f", ACTION)
            .unwrap()
            .unwrap();
        assert!(
            updated.starts_with(USER_CONFIG),
            "existing text must be untouched"
        );
        let before: Table = USER_CONFIG.parse().unwrap();
        let after: Table = updated.parse().unwrap();
        for name in ["onboarding", "theme"] {
            assert_eq!(before[name], after[name]);
        }
        assert_eq!(before["keys"]["next_agent"], after["keys"]["next_agent"]);
        let commands = after["keys"]["command"].as_array().unwrap();
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0], before["keys"]["command"][0]);
        assert_eq!(commands[1]["key"].as_str(), Some("prefix+f"));
        assert_eq!(commands[1]["type"].as_str(), Some("plugin_action"));
        assert_eq!(commands[1]["command"].as_str(), Some(ACTION));
    }

    #[test]
    fn empty_and_unterminated_files_still_get_a_valid_binding() {
        for existing in ["", "onboarding = false"] {
            let updated = with_binding(existing, "prefix+f", ACTION).unwrap().unwrap();
            let table: Table = updated.parse().unwrap();
            assert_eq!(
                table["keys"]["command"][0]["key"].as_str(),
                Some("prefix+f")
            );
        }
    }

    #[test]
    fn an_existing_identical_binding_is_left_alone() {
        let once = with_binding(USER_CONFIG, "prefix+f", ACTION)
            .unwrap()
            .unwrap();
        assert_eq!(with_binding(&once, "prefix+f", ACTION), Ok(None));
        assert_eq!(with_binding(&once, "PREFIX+F", ACTION), Ok(None));
    }

    #[test]
    fn a_key_in_use_is_never_overwritten() {
        let other_command = with_binding(USER_CONFIG, "prefix+o", ACTION).unwrap_err();
        assert!(other_command.contains("already bound"), "{other_command}");
        let builtin = "[keys]\nnew_tab = \"prefix+f\"\n";
        assert!(with_binding(builtin, "prefix+f", ACTION)
            .unwrap_err()
            .contains("keys.new_tab"));
        let listed = with_binding(USER_CONFIG, "prefix+x", ACTION).unwrap_err();
        assert!(listed.contains("keys.next_agent"), "{listed}");
        let same_key_other_action =
            "[[keys.command]]\nkey = \"prefix+f\"\ntype = \"shell\"\ncommand = \"ls\"\n";
        assert!(with_binding(same_key_other_action, "prefix+f", ACTION).is_err());
    }

    // Herdr takes `key` as a string or a list, and every command field is optional.
    #[test]
    fn command_entries_in_any_valid_shape_are_checked_without_panicking() {
        let listed = "[[keys.command]]\nkey = [\"prefix+o\", \"prefix+f\"]\ncommand = \"ls\"\n";
        let err = with_binding(listed, "prefix+f", ACTION).unwrap_err();
        assert!(err.contains("already bound"), "{err}");

        let untyped = "[[keys.command]]\nkey = \"prefix+f\"\ncommand = \"echo hello\"\n";
        let err = with_binding(untyped, "prefix+f", ACTION).unwrap_err();
        assert!(err.contains("already bound"), "{err}");

        for odd in ["[[keys.command]]\ncommand = \"ls\"\n", "[[keys.command]]\n"] {
            let bound = with_binding(odd, "prefix+f", ACTION).unwrap().unwrap();
            assert!(bound.starts_with(odd) && bound.contains(ACTION), "{odd:?}");
        }
        // Not a Herdr config at all: refused, not a panic.
        assert!(with_binding("[keys]\ncommand = [1, \"x\"]\n", "prefix+f", ACTION).is_err());

        let ours = format!(
            "[[keys.command]]\nkey = [\"prefix+f\"]\ntype = \"plugin_action\"\ncommand = {ACTION:?}\n"
        );
        assert_eq!(with_binding(&ours, "prefix+f", ACTION), Ok(None));
    }

    #[test]
    fn a_file_that_is_not_toml_is_not_touched() {
        assert!(with_binding("[keys\nbroken", "prefix+f", ACTION).is_err());
    }

    #[test]
    fn binding_a_file_keeps_a_backup_and_changes_nothing_on_conflict() {
        let dir = TempDir::new("bind");
        let config = dir.0.join("config.toml");
        std::fs::write(&config, USER_CONFIG).unwrap();

        let said = bind(&config, "prefix+f", ACTION).unwrap();
        assert!(
            said.contains("Backup:") && said.contains("Reload"),
            "{said}"
        );
        let updated = std::fs::read_to_string(&config).unwrap();
        assert!(updated.starts_with(USER_CONFIG));
        let backups: Vec<_> = std::fs::read_dir(&dir.0)
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".bak"))
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(
            std::fs::read_to_string(backups[0].path()).unwrap(),
            USER_CONFIG
        );

        // Again: already there, so no second backup and no change.
        assert!(bind(&config, "prefix+f", ACTION)
            .unwrap()
            .contains("nothing changed"));
        assert_eq!(std::fs::read_to_string(&config).unwrap(), updated);

        // A conflicting key: error, file byte-identical, no new backup.
        assert!(bind(&config, "prefix+o", ACTION).is_err());
        assert_eq!(std::fs::read_to_string(&config).unwrap(), updated);
        let after = std::fs::read_dir(&dir.0).unwrap().flatten().count();
        assert_eq!(after, 2, "only config.toml and one backup");
    }

    #[test]
    fn a_missing_config_is_an_error_not_a_new_file() {
        let dir = TempDir::new("bind-missing");
        let config = dir.0.join("config.toml");
        assert!(bind(&config, "prefix+f", ACTION).is_err());
        assert!(!config.exists());
    }
}
