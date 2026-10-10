//! Talks to Herdr through its CLI (`HERDR_BIN_PATH`) to open the popup and to
//! hand the chosen project to a shell.
//!
//! Herdr cannot change the directory of an existing pane, and typing `cd`
//! into one is unsafe (shell-specific quoting, busy shells). A project is
//! therefore only ever handed over by launching a *new* pane with
//! `--cwd`. No `pane run`, `send-text` or `send-keys` is used anywhere.

use crate::config::Direction;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const PICKER_ENTRYPOINT: &str = "picker";
pub const ENV_MODE: &str = "HERDR_PICKER_MODE";
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Manual action: open the project in a new split beside `pane`.
    Split,
    /// Auto trigger: swap `pane`, a pristine idle shell, for one in the project.
    Replace,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Split => "split",
            Mode::Replace => "replace",
        }
    }

    pub fn parse(text: &str) -> Option<Mode> {
        [Mode::Split, Mode::Replace]
            .into_iter()
            .find(|mode| mode.as_str() == text)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum HandoffError {
    /// Nothing was changed because it would not be safe.
    Refused(String),
    Failed(String),
}

impl std::fmt::Display for HandoffError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HandoffError::Refused(why) | HandoffError::Failed(why) => f.write_str(why),
        }
    }
}

/// Opens the picker popup, remembering the mode and the pane it acts on.
pub fn open_popup(
    herdr: &dyn Herdr,
    plugin_id: &str,
    mode: Mode,
    pane: &str,
) -> Result<(), String> {
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
            &format!("{ENV_MODE}={}", mode.as_str()),
            "--env",
            &format!("{ENV_PANE}={pane}"),
        ]))
        .map(drop)
}

/// Hands `project` to a new shell according to `mode`.
pub fn open_project(
    herdr: &dyn Herdr,
    mode: Mode,
    pane: &str,
    project: &Path,
    direction: Direction,
) -> Result<(), HandoffError> {
    if mode == Mode::Replace {
        ensure_pristine_shell(herdr, pane)?;
    }
    let cwd = project.to_string_lossy();
    herdr
        .run(&args(&[
            "pane",
            "split",
            "--pane",
            pane,
            "--direction",
            direction.as_str(),
            "--cwd",
            &cwd,
            "--focus",
        ]))
        .map_err(HandoffError::Failed)?;
    if mode == Mode::Replace {
        herdr
            .run(&args(&["pane", "close", pane]))
            .map_err(HandoffError::Failed)?;
    }
    Ok(())
}

/// A pane may be swapped out only when it is a plain shell sitting idle: no
/// agent, no plugin label, and the shell itself is the foreground process.
/// Anything Herdr cannot report counts as not idle.
fn ensure_pristine_shell(herdr: &dyn Herdr, pane: &str) -> Result<(), HandoffError> {
    let refuse = |why: &str| HandoffError::Refused(format!("left {pane} alone: {why}"));
    let query = |command: &[&str]| -> Result<Value, HandoffError> {
        let out = herdr
            .run(&args(command))
            .map_err(|err| refuse(&format!("cannot inspect it ({err})")))?;
        serde_json::from_str(&out).map_err(|err| refuse(&format!("unreadable reply ({err})")))
    };

    let info = query(&["pane", "get", pane])?;
    if !info["result"]["pane"]["agent"].is_null() {
        return Err(refuse("an agent owns it"));
    }
    if !info["result"]["pane"]["label"].is_null() {
        return Err(refuse("it is a labeled pane"));
    }
    let processes = query(&["pane", "process-info", "--pane", pane])?;
    let process_info = &processes["result"]["process_info"];
    let shell = process_info["shell_pid"].as_u64();
    let foreground = process_info["foreground_processes"].as_array();
    match (shell, foreground) {
        (Some(shell), Some(list)) if list.len() == 1 && list[0]["pid"].as_u64() == Some(shell) => {
            Ok(())
        }
        _ => Err(refuse("its shell is busy or Herdr cannot tell")),
    }
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

        fn commands(&self) -> Vec<String> {
            self.calls
                .borrow()
                .iter()
                .map(|c| c[..2].join(" "))
                .collect()
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

    const IDLE_PANE: &str = r#"{"result":{"type":"pane_info","pane":{"pane_id":"w1:p2"}}}"#;
    const IDLE_PROCESSES: &str = r#"{"result":{"type":"pane_process_info","process_info":{"pane_id":"w1:p2","shell_pid":100,"foreground_processes":[{"pid":100,"name":"bash"}]}}}"#;

    fn idle_shell() -> Mock {
        Mock::default()
            .reply("pane get", Ok(IDLE_PANE))
            .reply("pane process-info", Ok(IDLE_PROCESSES))
    }

    fn no_input_was_sent(mock: &Mock) {
        for command in mock.commands() {
            assert!(
                !["pane run", "pane send-text", "pane send-keys"].contains(&command.as_str()),
                "unsafe input command used: {command}"
            );
        }
    }

    fn replace(mock: &Mock) -> Result<(), HandoffError> {
        open_project(
            mock,
            Mode::Replace,
            "w1:p2",
            Path::new("/work/a"),
            Direction::Right,
        )
    }

    #[test]
    fn mode_round_trips_and_rejects_unknown_values() {
        assert_eq!(Mode::parse("split"), Some(Mode::Split));
        assert_eq!(Mode::parse(Mode::Replace.as_str()), Some(Mode::Replace));
        assert_eq!(Mode::parse("send-text"), None);
    }

    #[test]
    fn popup_is_opened_with_mode_and_pane_but_no_shell() {
        let mock = Mock::default();
        open_popup(&mock, "local.project-picker", Mode::Replace, "w1:p3").unwrap();
        let call = mock.calls().remove(0).join(" ");
        assert!(
            call.starts_with("plugin pane open --plugin local.project-picker --entrypoint picker")
        );
        assert!(call.contains("--placement popup"));
        assert!(call.contains("--env HERDR_PICKER_MODE=replace"));
        assert!(call.contains("--env HERDR_PICKER_PANE=w1:p3"));
    }

    #[test]
    fn split_mode_launches_a_new_shell_in_the_project() {
        let mock = Mock::default();
        let project = Path::new("/work/my project");
        open_project(&mock, Mode::Split, "w1:p2", project, Direction::Down).unwrap();
        assert_eq!(
            mock.calls(),
            [[
                "pane",
                "split",
                "--pane",
                "w1:p2",
                "--direction",
                "down",
                "--cwd",
                "/work/my project",
                "--focus"
            ]]
        );
        no_input_was_sent(&mock);
    }

    #[test]
    fn replace_mode_swaps_an_idle_shell_for_one_in_the_project() {
        let mock = idle_shell();
        replace(&mock).unwrap();
        assert_eq!(
            mock.commands(),
            ["pane get", "pane process-info", "pane split", "pane close"]
        );
        let calls = mock.calls();
        assert!(calls[2].contains(&"/work/a".to_string()));
        assert_eq!(calls[3], ["pane", "close", "w1:p2"]);
        no_input_was_sent(&mock);
    }

    #[test]
    fn replace_mode_leaves_a_busy_shell_alone() {
        let busy = r#"{"result":{"process_info":{"shell_pid":100,"foreground_processes":[{"pid":210,"name":"vim"}]}}}"#;
        let two = r#"{"result":{"process_info":{"shell_pid":100,"foreground_processes":[{"pid":100,"name":"bash"},{"pid":210,"name":"vim"}]}}}"#;
        let unknown = r#"{"result":{"process_info":{"pane_id":"w1:p2"}}}"#;
        for processes in [busy, two, unknown] {
            let mock = Mock::default()
                .reply("pane get", Ok(IDLE_PANE))
                .reply("pane process-info", Ok(processes));
            assert!(
                matches!(replace(&mock), Err(HandoffError::Refused(_))),
                "{processes}"
            );
            assert_eq!(mock.commands(), ["pane get", "pane process-info"]);
        }
    }

    #[test]
    fn replace_mode_leaves_agent_and_labeled_panes_alone() {
        for pane in [
            r#"{"result":{"pane":{"agent":"claude"}}}"#,
            r#"{"result":{"pane":{"label":"Picker"}}}"#,
        ] {
            let mock = Mock::default()
                .reply("pane get", Ok(pane))
                .reply("pane process-info", Ok(IDLE_PROCESSES));
            assert!(
                matches!(replace(&mock), Err(HandoffError::Refused(_))),
                "{pane}"
            );
            assert!(!mock.commands().contains(&"pane split".to_string()));
        }
    }

    #[test]
    fn replace_mode_keeps_the_pane_when_the_new_one_cannot_be_created() {
        let mock = idle_shell().reply("pane split", Err("no space"));
        assert_eq!(replace(&mock), Err(HandoffError::Failed("no space".into())));
        assert!(!mock.commands().contains(&"pane close".to_string()));
    }

    #[test]
    fn replace_mode_stops_when_the_pane_cannot_be_inspected() {
        let mock = Mock::default().reply("pane get", Err("pane not found"));
        assert!(matches!(replace(&mock), Err(HandoffError::Refused(_))));
        assert_eq!(mock.commands(), ["pane get"]);
    }
}
