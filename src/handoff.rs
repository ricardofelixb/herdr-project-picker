//! Talks to Herdr through its CLI (`HERDR_BIN_PATH`) to open the popup and to
//! hand the chosen project to a shell.
//!
//! Herdr cannot change the directory of an existing pane, and typing `cd`
//! into one is unsafe (shell-specific quoting, busy shells). A project is
//! therefore only ever handed over by launching a *new* pane with `--cwd`.
//! Existing panes are never typed into, restarted or closed: no `pane run`,
//! `send-text`, `send-keys` or `close` is used anywhere.

use crate::config::Direction;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const PICKER_ENTRYPOINT: &str = "picker";
pub const ENV_PANE: &str = "HERDR_PICKER_PANE";

pub trait Herdr {
    /// Runs `herdr <args>`; returns stdout, or the failure text.
    fn run(&self, args: &[String]) -> Result<String, String>;
}

pub struct HerdrCli(pub PathBuf);

impl Herdr for HerdrCli {
    fn run(&self, args: &[String]) -> Result<String, String> {
        let output = Command::new(&self.0)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|err| format!("{}: {err}", self.0.display()))?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = serde_json::from_str::<Value>(stderr.trim())
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(str::to_string));
        Err(message.unwrap_or_else(|| format!("herdr {}: {}", output.status, stderr.trim())))
    }
}

/// Opens the picker popup, remembering the pane the project opens beside.
pub fn open_popup(herdr: &dyn Herdr, plugin_id: &str, pane: &str) -> Result<(), String> {
    herdr
        .run(&args(&[
            "plugin",
            "pane",
            "open",
            "--plugin",
            plugin_id,
            "--entrypoint",
            PICKER_ENTRYPOINT,
            "--placement",
            "popup",
            "--width",
            "70%",
            "--height",
            "60%",
            "--env",
            &format!("{ENV_PANE}={pane}"),
        ]))
        .map(drop)
}

/// Opens `project` in a new shell split beside `pane`.
///
/// The split is created unfocused: a focused, unlabeled pane with an older
/// sibling is exactly what the `pane.created` filter accepts, so the picker
/// would reopen for its own pane. The 0.9.3 CLI focuses only by direction (the
/// socket's `pane.focus` by id has no CLI wrapper), so focus then moves by
/// direction from `pane`, whose neighbor the split is.
pub fn open_project(
    herdr: &dyn Herdr,
    pane: &str,
    project: &Path,
    direction: Direction,
) -> Result<(), String> {
    let cwd = project.to_string_lossy();
    herdr.run(&args(&[
        "pane",
        "split",
        "--pane",
        pane,
        "--direction",
        direction.as_str(),
        "--cwd",
        &cwd,
        "--no-focus",
    ]))?;
    // Cosmetic, hence best effort: the project is already open.
    let _ = herdr.run(&args(&[
        "pane",
        "focus",
        "--direction",
        direction.as_str(),
        "--pane",
        pane,
    ]));
    Ok(())
}

fn args(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| part.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Mock {
        calls: RefCell<Vec<Vec<String>>>,
        replies: Vec<(&'static str, Result<String, String>)>,
    }

    impl Mock {
        fn reply(mut self, command: &'static str, reply: Result<&str, &str>) -> Mock {
            self.replies
                .push((command, reply.map(String::from).map_err(String::from)));
            self
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.borrow().clone()
        }
    }

    impl Herdr for Mock {
        fn run(&self, args: &[String]) -> Result<String, String> {
            self.calls.borrow_mut().push(args.to_vec());
            let command = args[..2].join(" ");
            self.replies
                .iter()
                .find(|(c, _)| *c == command)
                .map(|(_, r)| r.clone())
                .unwrap_or(Ok(String::new()))
        }
    }

    /// Existing panes are only ever read-free: nothing typed, restarted or closed.
    fn existing_panes_untouched(mock: &Mock) {
        for call in mock.calls() {
            let command = call[..2].join(" ");
            assert!(
                !["pane run", "pane send-text", "pane send-keys", "pane close"]
                    .contains(&command.as_str()),
                "unsafe command used: {command}"
            );
        }
    }

    #[test]
    fn popup_is_opened_with_the_target_pane_but_no_shell() {
        let mock = Mock::default();
        open_popup(&mock, "local.project-picker", "w1:p3").unwrap();
        let call = mock.calls().remove(0).join(" ");
        assert!(
            call.starts_with("plugin pane open --plugin local.project-picker --entrypoint picker")
        );
        assert!(call.contains("--placement popup"));
        assert!(call.contains("--env HERDR_PICKER_PANE=w1:p3"));
    }

    // The split must not be created focused: see `open_project`.
    #[test]
    fn project_opens_in_an_unfocused_new_shell_then_focus_moves_to_it() {
        let mock = Mock::default();
        let project = Path::new("/work/my project");
        open_project(&mock, "w1:p2", project, Direction::Down).unwrap();
        assert_eq!(
            mock.calls(),
            [
                vec![
                    "pane",
                    "split",
                    "--pane",
                    "w1:p2",
                    "--direction",
                    "down",
                    "--cwd",
                    "/work/my project",
                    "--no-focus"
                ],
                vec!["pane", "focus", "--direction", "down", "--pane", "w1:p2"],
            ]
        );
        existing_panes_untouched(&mock);
    }

    #[test]
    fn a_failed_focus_move_does_not_fail_the_handoff() {
        let mock = Mock::default().reply("pane focus", Err("no neighbor"));
        let project = Path::new("/work/a");
        assert_eq!(
            open_project(&mock, "w1:p2", project, Direction::Right),
            Ok(())
        );
    }

    #[test]
    fn a_failed_split_is_reported_and_nothing_else_runs() {
        let mock = Mock::default().reply("pane split", Err("no space"));
        let project = Path::new("/work/a");
        assert_eq!(
            open_project(&mock, "w1:p2", project, Direction::Right),
            Err("no space".to_string())
        );
        assert_eq!(mock.calls().len(), 1);
    }
}
