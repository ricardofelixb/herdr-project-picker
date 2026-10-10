//! Project rows, and the project metadata inside `herdr api snapshot`.
//!
//! A remote machine may run another OS, so paths here are plain strings that
//! are never interpreted with the local platform's `Path` rules.

use serde_json::Value;

/// Where a project opens on its own machine. Pane and workspace ids belong to
/// one Herdr server, so they travel together with the machine they came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Destination {
    /// Opaque saved-profile id; `None` is the machine running the plugin.
    pub machine: Option<String>,
    pub pane_id: String,
    pub workspace_id: String,
    pub workspace_label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub name: String,
    pub path: String,
    /// Absent for folders found under the configured roots.
    pub dest: Option<Destination>,
}

impl Row {
    pub fn new(path: &str, dest: Option<Destination>) -> Row {
        Row {
            name: name_of(path),
            path: path.to_string(),
            dest,
        }
    }
}

/// Absolute on any OS: `/x`, `C:\x`, `C:/x` or a UNC path.
pub fn is_absolute(path: &str) -> bool {
    let b = path.as_bytes();
    !path.contains('\0')
        && (path.starts_with('/')
            || path.starts_with("\\\\")
            || (b.len() >= 3
                && b[0].is_ascii_alphabetic()
                && b[1] == b':'
                && matches!(b[2], b'/' | b'\\')))
}

fn windows_style(path: &str) -> bool {
    path.starts_with("\\\\") || path.as_bytes().get(1) == Some(&b':')
}

/// Comparison key: Windows-style paths ignore case and separator kind.
fn key(path: &str) -> String {
    if windows_style(path) {
        path.replace('\\', "/").trim_end_matches('/').to_lowercase()
    } else {
        path.trim_end_matches('/').to_string()
    }
}

fn name_of(path: &str) -> String {
    let separators: &[char] = if windows_style(path) {
        &['/', '\\']
    } else {
        &['/']
    };
    match path.trim_end_matches(separators).rsplit(separators).next() {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => path.to_string(),
    }
}

fn is_inside(child: &str, parent: &str) -> bool {
    key(child).starts_with(&format!("{}/", key(parent)))
}

struct Pane<'a> {
    id: &'a str,
    cwd: Option<&'a str>,
    focused: bool,
}

/// One row per project folder Herdr knows on the machine behind `machine`.
///
/// A workspace's folder is its worktree checkout when it has one, otherwise
/// the working directories of its panes, with nested ones folded into the
/// enclosing one. The foreground process's directory is never used: it is
/// wherever a tool happens to run, not the project. Only identifiers, labels
/// and directories are read; titles and terminal text are never touched.
pub fn rows_from_snapshot(reply: &str, machine: Option<&str>) -> Result<Vec<Row>, String> {
    let reply: Value =
        serde_json::from_str(reply).map_err(|err| format!("unreadable snapshot: {err}"))?;
    if let Some(message) = reply["error"]["message"].as_str() {
        return Err(message.to_string());
    }
    let snapshot = &reply["result"]["snapshot"];
    let (Some(workspaces), Some(panes)) = (
        snapshot["workspaces"].as_array(),
        snapshot["panes"].as_array(),
    ) else {
        return Err("snapshot has no workspaces and panes".into());
    };

    let mut rows = Vec::new();
    for workspace in workspaces {
        let Some(workspace_id) = workspace["workspace_id"].as_str() else {
            continue;
        };
        let members: Vec<Pane> = panes
            .iter()
            .filter(|pane| pane["workspace_id"].as_str() == Some(workspace_id))
            .filter_map(|pane| {
                Some(Pane {
                    id: pane["pane_id"].as_str()?,
                    cwd: pane["cwd"].as_str().filter(|cwd| is_absolute(cwd)),
                    focused: pane["focused"].as_bool().unwrap_or(false),
                })
            })
            .collect();

        let mut folders: Vec<&str> = Vec::new();
        let candidates = workspace["worktree"]["checkout_path"]
            .as_str()
            .into_iter()
            .chain(members.iter().filter_map(|pane| pane.cwd))
            .filter(|path| is_absolute(path));
        for path in candidates {
            if !folders.iter().any(|seen| key(seen) == key(path)) {
                folders.push(path);
            }
        }
        let outer: Vec<&str> = folders
            .iter()
            .copied()
            .filter(|path| !folders.iter().any(|other| is_inside(path, other)))
            .collect();

        for folder in outer {
            let at_folder = |pane: &&Pane| pane.cwd.is_some_and(|cwd| key(cwd) == key(folder));
            let pane = members
                .iter()
                .filter(at_folder)
                .find(|pane| pane.focused)
                .or_else(|| members.iter().find(at_folder))
                .or_else(|| members.first());
            let Some(pane) = pane else { continue };
            rows.push(Row::new(
                folder,
                Some(Destination {
                    machine: machine.map(str::to_string),
                    pane_id: pane.id.to_string(),
                    workspace_id: workspace_id.to_string(),
                    workspace_label: workspace["label"].as_str().unwrap_or(workspace_id).into(),
                }),
            ));
        }
    }
    Ok(rows)
}

/// Drops repeated folders (the first occurrence wins) and sorts by name, so
/// the list is the same for the same inputs.
pub fn merge(rows: Vec<Row>) -> Vec<Row> {
    let mut seen = std::collections::HashSet::new();
    let mut rows: Vec<Row> = rows
        .into_iter()
        .filter(|row| seen.insert(key(&row.path)))
        .collect();
    rows.sort_by_cached_key(|row| (row.name.to_lowercase(), row.path.clone()));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn reply(workspaces: Value, panes: Value) -> String {
        json!({"id": "cli:api:snapshot", "result": {"snapshot": {
            "version": "0.9.3", "workspaces": workspaces, "panes": panes
        }}})
        .to_string()
    }

    fn workspace(id: &str, label: &str) -> Value {
        json!({"workspace_id": id, "label": label, "focused": false})
    }

    fn pane(id: &str, workspace: &str, cwd: &str) -> Value {
        json!({"pane_id": id, "workspace_id": workspace, "cwd": cwd, "focused": false})
    }

    fn rows(reply: &str) -> Vec<Row> {
        rows_from_snapshot(reply, Some("profile-a")).unwrap()
    }

    #[test]
    fn rows_keep_the_pane_and_workspace_ids_of_their_own_machine() {
        let found = rows(&reply(
            json!([workspace("w1", "api"), workspace("w2", "web")]),
            json!([
                pane("w1:p1", "w1", "/srv/api"),
                pane("w2:p1", "w2", "/srv/web")
            ]),
        ));
        let dest = |pane_id: &str, workspace_id: &str, label: &str| {
            Some(Destination {
                machine: Some("profile-a".into()),
                pane_id: pane_id.into(),
                workspace_id: workspace_id.into(),
                workspace_label: label.into(),
            })
        };
        assert_eq!(
            found,
            [
                Row::new("/srv/api", dest("w1:p1", "w1", "api")),
                Row::new("/srv/web", dest("w2:p1", "w2", "web")),
            ]
        );
        assert_eq!(found[0].name, "api");
        let local = rows_from_snapshot(
            &reply(
                json!([workspace("w1", "a")]),
                json!([pane("w1:p1", "w1", "/a")]),
            ),
            None,
        );
        assert_eq!(local.unwrap()[0].dest.as_ref().unwrap().machine, None);
    }

    #[test]
    fn the_foreground_directory_is_never_a_project() {
        let mut busy = pane("w1:p1", "w1", "/srv/api");
        busy["foreground_cwd"] = json!("/srv/api/node_modules/.bin");
        let found = rows(&reply(json!([workspace("w1", "api")]), json!([busy])));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, "/srv/api");
    }

    #[test]
    fn nested_pane_folders_fold_into_the_workspace_root_but_other_workspaces_stay() {
        let found = rows(&reply(
            json!([workspace("w1", "app"), workspace("w2", "tools")]),
            json!([
                pane("w1:p1", "w1", "/p/app/src"),
                pane("w1:p2", "w1", "/p/app"),
                pane("w2:p1", "w2", "/p/app/tools")
            ]),
        ));
        let paths: Vec<&str> = found.iter().map(|row| row.path.as_str()).collect();
        assert_eq!(paths, ["/p/app", "/p/app/tools"]);
        assert_eq!(found[0].dest.as_ref().unwrap().pane_id, "w1:p2");
    }

    #[test]
    fn a_worktree_checkout_is_the_stable_root() {
        let mut feature = workspace("w1", "feature");
        feature["worktree"] = json!({"repo_key": "k", "repo_name": "app", "repo_root": "/p/app",
            "checkout_path": "/w/app-feature", "is_linked_worktree": true});
        let found = rows(&reply(
            json!([feature]),
            json!([pane("w1:p1", "w1", "/w/app-feature/src/deep")]),
        ));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, "/w/app-feature");
        assert_eq!(found[0].name, "app-feature");
        assert_eq!(found[0].dest.as_ref().unwrap().pane_id, "w1:p1");
    }

    #[test]
    fn the_focused_pane_at_the_root_is_the_destination() {
        let mut focused = pane("w1:p2", "w1", "/p/app");
        focused["focused"] = json!(true);
        let found = rows(&reply(
            json!([workspace("w1", "app")]),
            json!([pane("w1:p1", "w1", "/p/app"), focused]),
        ));
        assert_eq!(found[0].dest.as_ref().unwrap().pane_id, "w1:p2");
    }

    #[test]
    fn absolute_means_absolute_on_any_os() {
        for path in ["/srv/x", "C:\\x", "c:/x", "\\\\host\\share\\x"] {
            assert!(is_absolute(path), "{path}");
        }
        for path in ["", "x", "./x", "../x", "~/x", "C:x", "C:", "/x\0y"] {
            assert!(!is_absolute(path), "{path:?}");
        }
        let found = rows(&reply(
            json!([workspace("w1", "a")]),
            json!([pane("w1:p1", "w1", "relative/dir")]),
        ));
        assert!(found.is_empty());
    }

    #[test]
    fn windows_paths_compare_without_case_or_separator_differences() {
        let found = merge(rows(&reply(
            json!([workspace("w1", "first"), workspace("w2", "second")]),
            json!([
                pane("w1:p1", "w1", "C:\\Projects\\App"),
                pane("w2:p1", "w2", "c:/projects/app/")
            ]),
        )));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, "C:\\Projects\\App");
        assert_eq!(found[0].name, "App");
        assert_eq!(found[0].dest.as_ref().unwrap().workspace_id, "w1");
    }

    #[test]
    fn titles_agents_and_terminal_text_are_never_copied() {
        let mut noisy = pane("w1:p1", "w1", "/srv/api");
        noisy["title"] = json!("SECRET-TITLE");
        noisy["terminal_title"] = json!("SECRET-TERMINAL");
        noisy["agent"] = json!("SECRET-AGENT");
        let found = rows(&reply(json!([workspace("w1", "api")]), json!([noisy])));
        assert!(!format!("{found:?}").contains("SECRET"));
    }

    #[test]
    fn error_replies_and_garbage_are_errors() {
        let refused =
            r#"{"error":{"code":"machine_unreachable","message":"ssh: refused"},"id":"x"}"#;
        assert_eq!(
            rows_from_snapshot(refused, None),
            Err("ssh: refused".into())
        );
        assert!(rows_from_snapshot("not json", None).is_err());
        assert!(rows_from_snapshot("{}", None).is_err());
        assert!(rows_from_snapshot(r#"{"result":{"snapshot":{"workspaces":[]}}}"#, None).is_err());
    }

    #[test]
    fn merge_drops_repeats_first_wins_and_sorts_by_name() {
        let on_pane = Some(Destination {
            machine: None,
            pane_id: "w1:p1".into(),
            workspace_id: "w1".into(),
            workspace_label: "zed".into(),
        });
        let merged = merge(vec![
            Row::new("/p/zed", on_pane.clone()),
            Row::new("/p/Alpha", None),
            Row::new("/p/zed/", None),
            Row::new("/q/alpha", None),
        ]);
        let paths: Vec<&str> = merged.iter().map(|row| row.path.as_str()).collect();
        assert_eq!(paths, ["/p/Alpha", "/q/alpha", "/p/zed"]);
        assert_eq!(merged[2].dest, on_pane);
    }
}
