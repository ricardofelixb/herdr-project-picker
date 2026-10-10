//! The machines the popup can open a project on, and loading one machine's
//! projects when it is first shown.

use crate::config::{discover, Config};
use crate::handoff::{routed, Herdr};
use crate::snapshot::{merge, plain_id, rows_from_snapshot, Row};
use serde::Deserialize;
use std::path::Path;

/// Label of the machine running the plugin. Herdr gives the plugin no name or
/// profile id for it, and a saved profile of the same host would be a guess.
pub const CURRENT_LABEL: &str = "Current";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Machine {
    /// Opaque saved-profile id used for routing; `None` is the current machine.
    pub id: Option<String>,
    pub label: String,
}

impl Machine {
    pub fn current() -> Machine {
        Machine {
            id: None,
            label: CURRENT_LABEL.into(),
        }
    }
}

#[derive(Deserialize)]
struct Profile {
    id: String,
    label: String,
    #[serde(default)]
    enabled: bool,
}

/// The enabled saved machines from `herdr machine list --json`, in Herdr's
/// order. This is the catalog of the server running the plugin. The persisted
/// `selected` flag says nothing about this window and is ignored, and the SSH
/// target is not read at all.
pub fn saved_machines(json: &str) -> Result<Vec<Machine>, String> {
    let profiles: Vec<Profile> =
        serde_json::from_str(json).map_err(|err| format!("unreadable machine list: {err}"))?;
    Ok(profiles
        .into_iter()
        .filter(|profile| profile.enabled && plain_id(&profile.id))
        .map(|profile| Machine {
            id: Some(profile.id),
            label: profile.label,
        })
        .collect())
}

/// The current machine, then the enabled saved ones in Herdr's order. The
/// catalog is the one of the server running this plugin; when it cannot be
/// read only the current machine is offered, with the reason.
pub fn available(herdr: &dyn Herdr) -> (Vec<Machine>, Option<String>) {
    let mut machines = vec![Machine::current()];
    let catalog = herdr
        .run(&routed(None, &["machine", "list", "--json"]))
        .and_then(|json| saved_machines(&json));
    match catalog {
        Ok(saved) => {
            machines.extend(saved);
            (machines, None)
        }
        Err(err) => (machines, Some(format!("Saved machines unavailable: {err}"))),
    }
}

/// What one machine's tab shows.
#[derive(Debug, PartialEq, Eq)]
pub struct Loaded {
    pub rows: Vec<Row>,
    /// Said instead of an empty list, or under a list that is incomplete.
    pub note: Option<String>,
}

impl Loaded {
    /// Puts `warning` first in the note, keeping what the note already said.
    pub fn with_warning(self, warning: Option<&str>) -> Loaded {
        let Some(warning) = warning else { return self };
        let note = match self.note {
            Some(note) => format!("{warning}\n{note}"),
            None => warning.to_string(),
        };
        Loaded {
            note: Some(note),
            ..self
        }
    }
}

/// Reads one machine's projects, once, when its tab is first shown.
///
/// The current machine merges the configured roots with the folders of Herdr's
/// workspaces, and keeps the roots if Herdr cannot be asked. A saved machine
/// is only ever asked through Herdr's own `--machine` bridge for the folders
/// of its open workspaces: nothing is scanned or installed there, and a
/// failure is an error, never a quiet fallback to this machine.
pub fn load(
    machine: &Machine,
    herdr: &dyn Herdr,
    config: &Config,
    config_file: &Path,
) -> Result<Loaded, String> {
    let id = machine.id.as_deref();
    let known = herdr
        .run(&routed(id, &["api", "snapshot"]))
        .and_then(|reply| rows_from_snapshot(&reply, id));
    if id.is_some() {
        let rows = merge(known?);
        let note = rows
            .is_empty()
            .then(|| format!("No workspace on {} has a project folder.", machine.label));
        return Ok(Loaded { rows, note });
    }
    let (mut rows, mut note) = match known {
        Ok(rows) => (rows, None),
        Err(err) => (
            Vec::new(),
            Some(format!("Herdr workspaces unavailable: {err}")),
        ),
    };
    rows.extend(
        discover(config)
            .iter()
            .filter_map(|project| project.path.to_str())
            .map(|path| Row::new(path, None)),
    );
    let rows = merge(rows);
    if rows.is_empty() && note.is_none() {
        note = Some(format!(
            "No projects found.\nSet `roots` in {}\nsee config.example.toml",
            config_file.display()
        ));
    }
    Ok(Loaded { rows, note })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handoff::mock::Mock;

    #[test]
    fn saved_machines_keep_herdrs_order_and_only_enabled_ones() {
        let json = r#"[
            {"id":"a1","label":"Mini","target":"secret-host","session":"default","enabled":true,"selected":true},
            {"id":"b2","label":"Old","target":"x","session":"default","enabled":false,"selected":false},
            {"id":"c3","label":"Orgo","target":"y","session":"default","enabled":true,"selected":false}
        ]"#;
        let machines = saved_machines(json).unwrap();
        let ids: Vec<_> = machines
            .iter()
            .map(|m| (m.id.as_deref(), m.label.as_str()))
            .collect();
        assert_eq!(ids, [(Some("a1"), "Mini"), (Some("c3"), "Orgo")]);
        assert!(!format!("{machines:?}").contains("secret-host"));
    }

    #[test]
    fn identical_labels_stay_distinct_by_opaque_id() {
        let json = r#"[{"id":"a1","label":"Dev","enabled":true},{"id":"b2","label":"Dev","enabled":true}]"#;
        let machines = saved_machines(json).unwrap();
        assert_eq!(machines.len(), 2);
        assert_ne!(machines[0].id, machines[1].id);
    }

    #[test]
    fn unusable_catalog_entries_and_output_are_not_guessed_at() {
        let json = r#"[{"id":"","label":"Blank","enabled":true},{"id":"a1","label":"NoFlag"},
            {"id":"--focus","label":"Option","enabled":true}]"#;
        assert!(saved_machines(json).unwrap().is_empty());
        assert!(saved_machines("[]").unwrap().is_empty());
        assert!(saved_machines("No saved SSH machines.").is_err());
        assert!(saved_machines(r#"{"error":"x"}"#).is_err());
    }

    #[test]
    fn the_current_machine_comes_first_and_an_unreadable_catalog_says_why() {
        let json = r#"[{"id":"a1","label":"Mini","enabled":true},{"id":"b2","label":"Orgo","enabled":true}]"#;
        let mock = Mock::default().reply("machine list --json", Ok(json));
        let (machines, warning) = available(&mock);
        let labels: Vec<_> = machines.iter().map(|m| m.label.as_str()).collect();
        assert_eq!(labels, ["Current", "Mini", "Orgo"]);
        assert_eq!((machines[0].id.as_deref(), warning), (None, None));
        assert_eq!(mock.joined(), ["machine list --json"]);

        let broken = Mock::default().reply("machine list", Err("herdr: not found"));
        let (machines, warning) = available(&broken);
        assert_eq!(machines, [Machine::current()]);
        assert!(warning.unwrap().contains("herdr: not found"));
    }

    #[test]
    fn a_warning_leads_the_note_without_hiding_the_projects() {
        let loaded = || Loaded {
            rows: vec![Row::new("/p/a", None)],
            note: None,
        };
        assert_eq!(loaded().with_warning(None), loaded());
        let warned = loaded().with_warning(Some("Saved machines unavailable: x"));
        assert_eq!(warned.rows, loaded().rows);
        assert_eq!(
            warned.note.as_deref(),
            Some("Saved machines unavailable: x")
        );
        let both = Loaded {
            rows: vec![],
            note: Some("No projects found.".into()),
        }
        .with_warning(Some("W"));
        assert_eq!(both.note.as_deref(), Some("W\nNo projects found."));
    }

    fn snapshot(cwd: &str) -> String {
        format!(
            r#"{{"result":{{"snapshot":{{"workspaces":[{{"workspace_id":"w1","label":"api"}}],
            "panes":[{{"pane_id":"w1:p1","workspace_id":"w1","cwd":"{cwd}"}}]}}}}}}"#
        )
    }

    fn here() -> Config {
        Config {
            projects: vec![std::env::temp_dir()],
            ..Config::default()
        }
    }

    #[test]
    fn a_saved_machine_is_asked_through_its_profile_and_only_shows_its_own_folders() {
        let mock =
            Mock::default().reply("--machine prof-b api snapshot", Ok(&snapshot("/srv/api")));
        let machine = Machine {
            id: Some("prof-b".into()),
            label: "Orgo".into(),
        };
        let loaded = load(&machine, &mock, &here(), Path::new("config.toml")).unwrap();
        assert_eq!(mock.joined(), ["--machine prof-b api snapshot"]);
        assert_eq!(
            loaded.rows.len(),
            1,
            "configured local roots leaked into a remote tab"
        );
        assert_eq!(loaded.rows[0].machine.as_deref(), Some("prof-b"));
        assert_eq!(loaded.note, None);
    }

    #[test]
    fn a_saved_machine_that_fails_is_an_error_not_a_local_list() {
        let machine = Machine {
            id: Some("prof-b".into()),
            label: "Orgo".into(),
        };
        for failure in [Err("timed out after 20s"), Ok("garbage")] {
            let mock = Mock::default().reply("--machine prof-b", failure);
            let err = load(&machine, &mock, &here(), Path::new("config.toml")).unwrap_err();
            assert!(!err.is_empty());
        }
    }

    #[test]
    fn an_empty_saved_machine_says_so() {
        let mock = Mock::default().reply(
            "--machine",
            Ok(r#"{"result":{"snapshot":{"workspaces":[],"panes":[]}}}"#),
        );
        let machine = Machine {
            id: Some("prof-b".into()),
            label: "Orgo".into(),
        };
        let loaded = load(&machine, &mock, &here(), Path::new("config.toml")).unwrap();
        assert!(loaded.rows.is_empty());
        assert!(loaded.note.unwrap().contains("Orgo"));
    }

    #[test]
    fn the_current_machine_merges_configured_roots_and_workspace_folders() {
        let dir = std::env::temp_dir();
        let dir_text = dir.to_str().unwrap().replace('\\', "\\\\");
        let mock = Mock::default().reply("api snapshot", Ok(&snapshot(&dir_text)));
        let loaded = load(
            &Machine::current(),
            &mock,
            &here(),
            Path::new("config.toml"),
        )
        .unwrap();
        assert_eq!(
            mock.joined(),
            ["api snapshot"],
            "no --machine for the current machine"
        );
        assert_eq!(loaded.rows.len(), 1, "the same folder must appear once");
        assert_eq!(loaded.note, None);
    }

    #[test]
    fn the_current_machine_keeps_its_roots_when_herdr_cannot_be_asked() {
        let mock = Mock::default().reply("api snapshot", Err("herdr: not found"));
        let loaded = load(
            &Machine::current(),
            &mock,
            &here(),
            Path::new("config.toml"),
        )
        .unwrap();
        assert_eq!(loaded.rows.len(), 1);
        assert!(loaded.note.unwrap().contains("herdr: not found"));
    }

    #[test]
    fn nothing_anywhere_points_at_the_config_file() {
        let mock = Mock::default().reply(
            "api snapshot",
            Ok(r#"{"result":{"snapshot":{"workspaces":[],"panes":[]}}}"#),
        );
        let loaded = load(
            &Machine::current(),
            &mock,
            &Config::default(),
            Path::new("/cfg/config.toml"),
        )
        .unwrap();
        assert!(loaded.rows.is_empty());
        assert!(loaded.note.unwrap().contains("config.toml"));
    }
}

/// Read-only checks against a running Herdr, for validating by hand:
/// `cargo test --locked live -- --ignored --nocapture`. Set
/// `HERDR_PICKER_LIVE_REMOTE=1` to also read each saved machine's snapshot
/// (Herdr's own non-interactive SSH bridge; nothing is created or changed).
#[cfg(test)]
mod live {
    use super::*;
    use crate::handoff::{HerdrCli, CALL_TIMEOUT};

    fn herdr() -> HerdrCli {
        HerdrCli {
            bin: std::env::var_os("HERDR_BIN_PATH").map_or_else(|| "herdr".into(), Into::into),
            timeout: CALL_TIMEOUT,
        }
    }

    #[test]
    #[ignore = "needs a running Herdr"]
    fn live_catalog_and_snapshots_parse() {
        let herdr = herdr();
        let json = herdr
            .run(&routed(None, &["machine", "list", "--json"]))
            .unwrap();
        let saved = saved_machines(&json).unwrap();
        println!(
            "saved enabled machines: {:?}",
            saved.iter().map(|m| &m.label).collect::<Vec<_>>()
        );

        let here = load(
            &Machine::current(),
            &herdr,
            &Config::default(),
            Path::new("config.toml"),
        )
        .unwrap();
        println!(
            "Current: {} projects, note {:?}",
            here.rows.len(),
            here.note
        );
        for row in &here.rows {
            assert!(crate::snapshot::is_absolute(&row.path), "{}", row.path);
            assert!(row.machine.is_none());
            println!("  {}", row.path);
        }
        assert!(
            !here.rows.is_empty(),
            "a running Herdr has at least one workspace"
        );

        if std::env::var_os("HERDR_PICKER_LIVE_REMOTE").is_none() {
            return;
        }
        for machine in saved {
            match load(
                &machine,
                &herdr,
                &Config::default(),
                Path::new("config.toml"),
            ) {
                Ok(loaded) => {
                    println!("{}: {} projects", machine.label, loaded.rows.len());
                    for row in &loaded.rows {
                        assert_eq!(row.machine, machine.id);
                        println!("  {}", row.path);
                    }
                }
                Err(err) => println!("{}: unavailable ({err})", machine.label),
            }
        }
    }
}
