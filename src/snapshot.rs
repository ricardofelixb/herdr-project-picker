//! Project rows, and the project metadata inside `herdr api snapshot`.
//!
//! A remote machine may run another OS, so paths here are plain strings that
//! are never interpreted with the local platform's `Path` rules.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub name: String,
    pub path: String,
    /// Opaque saved-profile id of the machine the folder was found on; `None`
    /// is the machine running the plugin. A row only ever opens there.
    pub machine: Option<String>,
}

impl Row {
    pub fn new(path: &str, machine: Option<&str>) -> Row {
        Row {
            name: name_of(path),
            path: path.to_string(),
            machine: machine.map(str::to_string),
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

/// Herdr profile ids put into a command line: letters, digits and `:_.-`,
/// never starting with `-` so they cannot read as options.
pub fn plain_id(id: &str) -> bool {
    !id.starts_with('-')
        && !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '.' | '-'))
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
        let cwds = panes
            .iter()
            .filter(|pane| pane["workspace_id"].as_str() == Some(workspace_id))
            .filter_map(|pane| pane["cwd"].as_str());

        let mut folders: Vec<&str> = Vec::new();
        let checkout = workspace["worktree"]["checkout_path"]
            .as_str()
            .filter(|path| is_absolute(path));
        for path in checkout.into_iter().chain(cwds).filter(|p| is_absolute(p)) {
            if !folders.iter().any(|seen| key(seen) == key(path)) {
                folders.push(path);
            }
        }
        // Herdr says which folder is the worktree's root, so a pane that sits in
        // one of its parents does not fold it away.
        let is_checkout = |path: &str| checkout.is_some_and(|root| key(root) == key(path));
        rows.extend(
            folders
                .iter()
                .filter(|path| {
                    is_checkout(path) || !folders.iter().any(|other| is_inside(path, other))
                })
                .map(|folder| Row::new(folder, machine)),
        );
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
    fn rows_belong_to_the_machine_they_were_read_from() {
        let found = rows(&reply(
            json!([workspace("w1", "api"), workspace("w2", "web")]),
            json!([
                pane("w1:p1", "w1", "/srv/api"),
                pane("w2:p1", "w2", "/srv/web")
            ]),
        ));
        assert_eq!(
            found,
            [
                Row::new("/srv/api", Some("profile-a")),
                Row::new("/srv/web", Some("profile-a")),
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
        assert_eq!(local.unwrap()[0].machine, None);
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
    }

    // A pane that wandered above the checkout must not hide the checkout.
    #[test]
    fn a_checkout_survives_a_pane_in_one_of_its_parent_folders() {
        let mut feature = workspace("w1", "feature");
        feature["worktree"] = json!({"checkout_path": "/home/me/app-feature"});
        let found = rows(&reply(
            json!([feature]),
            json!([
                pane("w1:p1", "w1", "/home/me"),
                pane("w1:p2", "w1", "/home/me/app-feature/src")
            ]),
        ));
        let paths: Vec<_> = found.iter().map(|row| row.path.as_str()).collect();
        assert_eq!(paths, ["/home/me/app-feature", "/home/me"]); // by name
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
        let merged = merge(vec![
            Row::new("/p/zed", Some("first")),
            Row::new("/p/Alpha", None),
            Row::new("/p/zed/", Some("second")),
            Row::new("/q/alpha", None),
        ]);
        let paths: Vec<&str> = merged.iter().map(|row| row.path.as_str()).collect();
        assert_eq!(paths, ["/p/Alpha", "/q/alpha", "/p/zed"]);
        assert_eq!(merged[2].machine.as_deref(), Some("first"));
    }
}
