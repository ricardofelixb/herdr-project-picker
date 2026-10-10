//! The machines the popup can open a project on, and loading one machine's
//! projects when it is first shown.

use crate::config::{discover, Config};
use crate::handoff::{routed, Herdr};
use crate::snapshot::{holds, is_absolute, merge, plain_id, rows_from_snapshot, Row};
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
    /// The saved profile's SSH target, used only to list configured roots.
    pub target: Option<String>,
}

impl Machine {
    pub fn current() -> Machine {
        Machine {
            id: None,
            label: CURRENT_LABEL.into(),
            target: None,
        }
    }
}

#[derive(Deserialize)]
struct Profile {
    id: String,
    label: String,
    #[serde(default)]
    target: String,
    #[serde(default)]
    enabled: bool,
}

/// The enabled saved machines from `herdr machine list --json`, in Herdr's
/// order. This is the catalog of the server running the plugin. The persisted
/// `selected` flag says nothing about this window and is ignored.
pub fn saved_machines(json: &str) -> Result<Vec<Machine>, String> {
    let profiles: Vec<Profile> =
        serde_json::from_str(json).map_err(|err| format!("unreadable machine list: {err}"))?;
    Ok(profiles
        .into_iter()
        .filter(|profile| profile.enabled && plain_id(&profile.id))
        .map(|profile| Machine {
            id: Some(profile.id),
            label: profile.label,
            // Passed to ssh as its destination: never something read as an option.
            target: Some(profile.target).filter(|target| {
                !target.is_empty()
                    && !target.starts_with('-')
                    && !target.chars().any(|c| c.is_whitespace() || c.is_control())
            }),
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
/// Every machine lists the folders of its open Herdr workspaces, minus the
/// ones that hold a configured root (a home folder is not a project), plus the
/// subfolders of its configured roots. The current machine reads its roots
/// itself and keeps them if Herdr cannot be asked. A saved machine is asked
/// through Herdr's own `--machine` bridge, and its configured roots are listed
/// read-only over SSH; a failed bridge is an error, never a quiet fallback to
/// this machine.
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
    if let Some(id) = id {
        let mut rows = known?;
        let mut note = None;
        let roots = config
            .machines
            .get(id)
            .or(config.machines.get(&machine.label));
        if let Some(roots) = roots {
            match list_remote(herdr, machine, roots) {
                Ok((expanded, found)) => {
                    rows.retain(|row| !expanded.iter().any(|root| holds(&row.path, root)));
                    rows.extend(found.iter().map(|path| Row::new(path, Some(id))));
                }
                Err(err) => note = Some(format!("Could not list {}: {err}", roots.join(", "))),
            }
        }
        let rows = merge(rows);
        if rows.is_empty() && note.is_none() {
            note = Some(format!(
                "No projects on {}.\nAdd [machines.{}] roots in {}",
                machine.label,
                machine.label,
                config_file.display()
            ));
        }
        return Ok(Loaded { rows, note });
    }
    let (mut rows, mut note) = match known {
        Ok(rows) => (rows, None),
        Err(err) => (
            Vec::new(),
            Some(format!("Herdr workspaces unavailable: {err}")),
        ),
    };
    let roots: Vec<&str> = config.roots.iter().filter_map(|r| r.to_str()).collect();
    rows.retain(|row| !roots.iter().any(|root| holds(&row.path, root)));
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

/// Run by `sh` on a saved machine, with the roots as arguments: prints each
/// root as that machine expands `~`, then its non-hidden subfolders. It holds
/// no single quote, because it travels inside one.
const LIST_SCRIPT: &str = r#"for r in "$@"; do case $r in "~") r=$HOME ;; "~/"*) r=$HOME/${r#"~/"} ;; esac; printf "root\t%s\n" "$r"; for d in "$r"/*/; do [ -d "$d" ] && printf "dir\t%s\n" "${d%/}"; done; done; true"#;

/// Lists `roots` on a saved machine over SSH, read-only. Returns the roots as
/// that machine expanded them, and the absolute folders found under them.
fn list_remote(
    herdr: &dyn Herdr,
    machine: &Machine,
    roots: &[String],
) -> Result<(Vec<String>, Vec<String>), String> {
    let target = (machine.target.as_deref()).ok_or("Herdr has no SSH target for it")?;
    // Config parsing guarantees the roots hold no single quote.
    let quoted: Vec<String> = roots.iter().map(|root| format!("'{root}'")).collect();
    let command = format!("sh -c '{LIST_SCRIPT}' sh {}", quoted.join(" "));
    let (mut expanded, mut found) = (Vec::new(), Vec::new());
    for line in herdr.ssh(target, &command)?.lines() {
        match line.split_once('\t') {
            Some(("root", path)) => expanded.push(path.to_string()),
            Some(("dir", path)) if is_absolute(path) => found.push(path.to_string()),
            _ => {}
        }
    }
    Ok((expanded, found))
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
        assert_eq!(machines[0].target.as_deref(), Some("secret-host"));
        let odd = r#"[{"id":"a1","label":"A","target":"-oProxyCommand=x","enabled":true},
            {"id":"b2","label":"B","target":"a b","enabled":true},{"id":"c3","label":"C","enabled":true}]"#;
        let odd = saved_machines(odd).unwrap();
        assert!(odd.iter().all(|m| m.target.is_none()), "{odd:?}");
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

    fn orgo() -> Machine {
        Machine {
            id: Some("prof-b".into()),
            label: "Orgo".into(),
            target: Some("orgo".into()),
        }
    }

    fn with_roots(roots: &[&str]) -> Config {
        let mut config = here();
        let roots = roots.iter().map(|root| root.to_string()).collect();
        config.machines.insert("Orgo".into(), roots);
        config
    }

    // Herdr's own panes there sit in the home folder; the configured roots
    // are what the user calls projects.
    #[test]
    fn a_saved_machines_roots_are_listed_over_ssh_and_the_home_folder_dropped() {
        let listing = "root\t/root/src/a\ndir\t/root/src/a/api\ndir\t/root/src/a/web\n\
                       root\t/root/src/b\ndir\t/root/src/b/api\nnoise\ndir\trelative\n";
        let mock = Mock::default()
            .reply("--machine prof-b api snapshot", Ok(&snapshot("/root")))
            .reply("ssh orgo", Ok(listing));
        let config = with_roots(&["~/src/a", "/root/src/b"]);
        let loaded = load(&orgo(), &mock, &config, Path::new("config.toml")).unwrap();
        let paths: Vec<_> = loaded.rows.iter().map(|row| row.path.as_str()).collect();
        assert_eq!(
            paths,
            ["/root/src/a/api", "/root/src/b/api", "/root/src/a/web"]
        );
        assert!(loaded
            .rows
            .iter()
            .all(|r| r.machine.as_deref() == Some("prof-b")));
        assert_eq!(loaded.note, None);
        let ssh = &mock.calls()[1];
        assert_eq!(ssh[..2], ["ssh", "orgo"]);
        assert!(ssh[2].starts_with("sh -c '") && ssh[2].ends_with("' sh '~/src/a' '/root/src/b'"));
        assert_eq!(
            ssh[2].matches('\'').count(),
            6,
            "the script must not end its quote"
        );
    }

    #[test]
    fn a_failed_listing_keeps_the_workspace_folders_and_says_why() {
        let mock = Mock::default()
            .reply("--machine prof-b api snapshot", Ok(&snapshot("/srv/api")))
            .reply("ssh orgo", Err("ssh exit code: 255: Permission denied"));
        let config = with_roots(&["~/src"]);
        let loaded = load(&orgo(), &mock, &config, Path::new("config.toml")).unwrap();
        assert_eq!(loaded.rows.len(), 1);
        assert!(loaded.note.unwrap().contains("Permission denied"));

        let mut unknown = orgo();
        unknown.target = None;
        let loaded = load(&unknown, &mock, &config, Path::new("config.toml")).unwrap();
        assert!(loaded.note.unwrap().contains("no SSH target"));
    }

    #[test]
    fn machines_without_roots_are_never_contacted_over_ssh() {
        let mock = Mock::default().reply("--machine", Ok(&snapshot("/srv/api")));
        load(&orgo(), &mock, &here(), Path::new("config.toml")).unwrap();
        assert!(mock.joined().iter().all(|call| !call.starts_with("ssh")));
    }

    // The local roots also drop the local workspace that sits above them.
    #[test]
    fn the_current_machines_home_folder_is_not_a_project() {
        let root = crate::config::tempdir::TempDir::new("home-drop");
        std::fs::create_dir(root.0.join("app")).unwrap();
        // JSON-escaped for the snapshot text.
        let parent = root
            .0
            .parent()
            .unwrap()
            .to_str()
            .unwrap()
            .replace('\\', "\\\\");
        let mock = Mock::default().reply("api snapshot", Ok(&snapshot(&parent)));
        let config = Config {
            roots: vec![root.0.clone()],
            ..Config::default()
        };
        let loaded = load(&Machine::current(), &mock, &config, Path::new("c")).unwrap();
        let names: Vec<_> = loaded.rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, ["app"]);
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
        let machine = orgo();
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
        let machine = orgo();
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
        let machine = orgo();
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
        // The real plugin config when given, so configured roots are listed too.
        let home = std::env::var_os("USERPROFILE").or(std::env::var_os("HOME"));
        let config = std::env::var_os("HERDR_PLUGIN_CONFIG_DIR")
            .map_or_else(Config::default, |dir| {
                Config::load(Path::new(&dir), None, home.as_deref().map(Path::new)).unwrap()
            });
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
            &config,
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
            match load(&machine, &herdr, &config, Path::new("config.toml")) {
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
