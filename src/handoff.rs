//! Talks to Herdr through its CLI (`HERDR_BIN_PATH`) to open the popup and to
//! open the chosen project as a new workspace.
//!
//! A project is only ever opened by creating a *new* workspace whose shell
//! starts in the project (`--cwd`). Existing panes are never typed into,
//! restarted or closed: no `pane run`, `send-text`, `send-keys` or `close` is
//! used anywhere.
//!
//! Every command for a saved machine is routed with
//! `--machine <opaque profile id>`.

use crate::machine::Machine;
use crate::snapshot::{is_absolute, Row};
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

pub const PICKER_ENTRYPOINT: &str = "picker";

/// How long one Herdr call may take. Remote calls go through Herdr's own
/// non-interactive SSH bridge, which fails rather than prompting.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(20);

pub trait Herdr {
    /// Runs `herdr <args>`; returns stdout, or the failure text.
    fn run(&self, args: &[String]) -> Result<String, String>;
    /// Runs `command` on a saved machine through OpenSSH, never prompting.
    fn ssh(&self, target: &str, command: &str) -> Result<String, String>;
}

#[derive(Clone)]
pub struct HerdrCli {
    pub bin: PathBuf,
    pub timeout: Duration,
}

impl Herdr for HerdrCli {
    fn run(&self, args: &[String]) -> Result<String, String> {
        run_bounded(&self.bin, args, self.timeout)
    }

    fn ssh(&self, target: &str, command: &str) -> Result<String, String> {
        let args = [
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            "--",
            target,
            command,
        ];
        let args: Vec<String> = args.map(String::from).to_vec();
        run_bounded(Path::new("ssh"), &args, self.timeout)
    }
}

/// Runs a program to completion, or kills it once `timeout` has passed. The
/// deadline is a blocking wait on the output readers, not a polling loop.
pub fn run_bounded(program: &Path, args: &[String], timeout: Duration) -> Result<String, String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("{}: {err}", program.display()))?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let within = |rx: &Receiver<Vec<u8>>| {
        rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
    };
    let (Ok(stdout), Ok(stderr)) = (within(&stdout), within(&stderr)) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("timed out after {timeout:?}"));
    };
    let status = child.wait().map_err(|err| err.to_string())?;
    if status.success() {
        return Ok(String::from_utf8_lossy(&stdout).into_owned());
    }
    let name = program.file_stem().unwrap_or_default().to_string_lossy();
    Err(explain(
        &format!("{name} {status}"),
        &String::from_utf8_lossy(&stderr),
    ))
}

/// Herdr reports failures as `{"error":{"message":...}}` on stderr.
fn explain(what: &str, stderr: &str) -> String {
    serde_json::from_str::<Value>(stderr.trim())
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{what}: {}", stderr.trim()))
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        let _ = tx.send(bytes);
    });
    rx
}

/// `herdr [--machine <id>] <args>`. The saved profile's opaque id is the only
/// selector used: labels are not unique and never identify a machine.
pub fn routed(machine: Option<&str>, args: &[&str]) -> Vec<String> {
    machine
        .into_iter()
        .flat_map(|id| ["--machine", id])
        .chain(args.iter().copied())
        .map(str::to_string)
        .collect()
}

/// Opens the picker popup; its size and placement come from the manifest.
pub fn open_popup(herdr: &dyn Herdr, plugin_id: &str) -> Result<(), String> {
    herdr
        .run(&routed(
            None,
            &[
                "plugin",
                "pane",
                "open",
                "--plugin",
                plugin_id,
                "--entrypoint",
                PICKER_ENTRYPOINT,
            ],
        ))
        .map(drop)
}

/// Opens `row` as a new, focused workspace named after the project, with its
/// shell in the project folder, on the machine the row was found on.
///
/// Herdr cannot switch this window to another machine, so for a saved machine
/// the workspace becomes the one shown there (focus is per server: other
/// windows on that machine follow) and a Herdr toast says where it went.
pub fn open_project(herdr: &dyn Herdr, machine: &Machine, row: &Row) -> Result<(), String> {
    if !is_absolute(&row.path) {
        return Err(format!("{} is not an absolute path", row.path));
    }
    if row.machine != machine.id {
        return Err(format!(
            "{} does not belong to {}; nothing was opened",
            row.path, machine.label
        ));
    }
    let args = [
        "workspace",
        "create",
        "--cwd",
        &row.path,
        "--label",
        &row.name,
        "--focus",
    ];
    herdr.run(&routed(machine.id.as_deref(), &args))?;
    if machine.id.is_some() {
        // Cosmetic, hence best effort: the workspace already exists.
        let title = format!("Opened {} on {}", row.name, machine.label);
        let body = format!("Select {} in the sidebar.", machine.label);
        let toast = ["notification", "show", &title, "--body", &body];
        let _ = herdr.run(&routed(None, &toast));
    }
    Ok(())
}

/// A scripted stand-in for the Herdr CLI, shared by the tests of the modules
/// that call it.
#[cfg(test)]
pub mod mock {
    use super::Herdr;
    use std::cell::RefCell;

    #[derive(Default)]
    pub struct Mock {
        calls: RefCell<Vec<Vec<String>>>,
        replies: Vec<(String, Result<String, String>)>,
    }

    impl Mock {
        /// Answers calls whose joined arguments start with `prefix`.
        pub fn reply(mut self, prefix: &str, reply: Result<&str, &str>) -> Mock {
            let reply = reply.map(String::from).map_err(String::from);
            self.replies.push((prefix.to_string(), reply));
            self
        }

        pub fn calls(&self) -> Vec<Vec<String>> {
            self.calls.borrow().clone()
        }

        pub fn joined(&self) -> Vec<String> {
            self.calls().iter().map(|call| call.join(" ")).collect()
        }
    }

    impl Herdr for Mock {
        fn run(&self, args: &[String]) -> Result<String, String> {
            self.calls.borrow_mut().push(args.to_vec());
            let line = args.join(" ");
            self.replies
                .iter()
                .find(|(prefix, _)| line.starts_with(prefix.as_str()))
                .map(|(_, reply)| reply.clone())
                .unwrap_or(Ok(String::new()))
        }

        /// Recorded as `ssh <target> <command>`.
        fn ssh(&self, target: &str, command: &str) -> Result<String, String> {
            self.run(&["ssh".into(), target.into(), command.into()])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mock::Mock;
    use super::*;

    fn remote(id: &str) -> Machine {
        Machine {
            id: Some(id.into()),
            label: "Orgo".into(),
            target: None,
        }
    }

    /// Nothing is ever typed into, restarted, closed or split.
    fn only_new_workspaces(mock: &Mock) {
        for line in mock.joined() {
            for forbidden in [
                "pane run",
                "pane send-text",
                "pane send-keys",
                "pane close",
                "pane split",
            ] {
                assert!(!line.contains(forbidden), "unsafe: {line}");
            }
        }
    }

    #[test]
    fn popup_is_opened_by_its_entrypoint_alone() {
        let mock = Mock::default();
        open_popup(&mock, "local.project-picker").unwrap();
        assert_eq!(
            mock.joined(),
            ["plugin pane open --plugin local.project-picker --entrypoint picker"]
        );
    }

    #[test]
    fn local_project_opens_as_a_focused_workspace_named_after_it() {
        let mock = Mock::default();
        let row = Row::new("/work/my project", None);
        assert_eq!(open_project(&mock, &Machine::current(), &row), Ok(()));
        assert_eq!(
            mock.calls(),
            [[
                "workspace",
                "create",
                "--cwd",
                "/work/my project",
                "--label",
                "my project",
                "--focus"
            ]]
        );
        only_new_workspaces(&mock);
    }

    // Herdr's parser takes the next argument as the value, so a folder named
    // like an option stays a value.
    #[test]
    fn a_folder_named_like_an_option_stays_one_argument() {
        let mock = Mock::default();
        let row = Row::new("/work/--focus", None);
        open_project(&mock, &Machine::current(), &row).unwrap();
        let call = &mock.calls()[0];
        assert_eq!(call[4..6], ["--label", "--focus"]);
        assert_eq!(call.len(), 7);
    }

    #[test]
    fn a_failed_create_is_reported() {
        let mock = Mock::default().reply("workspace create", Err("no such folder"));
        let row = Row::new("/work/a", None);
        let done = open_project(&mock, &Machine::current(), &row);
        assert_eq!(done, Err("no such folder".to_string()));
    }

    // Herdr cannot switch this window to another machine: the workspace is
    // focused there, and a toast on this machine says where it went.
    #[test]
    fn remote_project_is_routed_by_profile_id_focused_there_and_announced_here() {
        let mock = Mock::default();
        let row = Row::new("/srv/api", Some("prof-b"));
        open_project(&mock, &remote("prof-b"), &row).unwrap();
        assert_eq!(
            mock.joined(),
            [
                "--machine prof-b workspace create --cwd /srv/api --label api --focus",
                "notification show Opened api on Orgo --body Select Orgo in the sidebar.",
            ]
        );
        only_new_workspaces(&mock);
    }

    #[test]
    fn a_failed_toast_does_not_fail_the_open() {
        let mock = Mock::default().reply("notification", Err("ui busy"));
        let row = Row::new("/srv/api", Some("prof-b"));
        assert_eq!(open_project(&mock, &remote("prof-b"), &row), Ok(()));
    }

    #[test]
    fn a_row_is_never_opened_on_a_machine_it_was_not_loaded_from() {
        let mock = Mock::default();
        let from_a = Row::new("/srv/api", Some("prof-a"));
        let local = Row::new("/work/api", None);
        let cases = [
            (remote("prof-b"), &from_a),
            (Machine::current(), &from_a),
            (remote("prof-a"), &local),
        ];
        for (machine, row) in cases {
            let err = open_project(&mock, &machine, row).unwrap_err();
            assert!(err.contains("does not belong"), "{err}");
        }
        assert!(mock.calls().is_empty(), "{:?}", mock.joined());
    }

    #[test]
    fn an_unavailable_machine_is_an_error_not_a_local_fallback() {
        let mock = Mock::default().reply("--machine", Err("ssh: connection refused"));
        let row = Row::new("/srv/api", Some("prof-b"));
        let err = open_project(&mock, &remote("prof-b"), &row).unwrap_err();
        assert!(err.contains("connection refused"), "{err}");
        assert_eq!(
            mock.calls().len(),
            1,
            "nothing is announced after a failure"
        );
    }

    #[test]
    fn relative_paths_never_reach_herdr() {
        let mock = Mock::default();
        for (machine, row) in [
            (Machine::current(), Row::new("projects/api", None)),
            (remote("prof-b"), Row::new("~/api", Some("prof-b"))),
        ] {
            assert!(open_project(&mock, &machine, &row).is_err());
        }
        assert!(mock.calls().is_empty());
    }

    #[test]
    fn routing_uses_the_opaque_id_only_when_a_machine_is_given() {
        assert_eq!(routed(None, &["api", "snapshot"]), ["api", "snapshot"]);
        assert_eq!(
            routed(Some("c39b"), &["api", "snapshot"]),
            ["--machine", "c39b", "api", "snapshot"]
        );
    }

    #[cfg(windows)]
    fn sleeper() -> (PathBuf, Vec<String>) {
        (
            "ping".into(),
            ["-n", "30", "127.0.0.1"].map(String::from).to_vec(),
        )
    }

    #[cfg(not(windows))]
    fn sleeper() -> (PathBuf, Vec<String>) {
        ("sleep".into(), vec!["30".into()])
    }

    #[cfg(windows)]
    fn echo_and_fail() -> (PathBuf, Vec<String>) {
        (
            "cmd".into(),
            ["/C", "echo boom 1>&2 & exit 3"].map(String::from).to_vec(),
        )
    }

    #[cfg(not(windows))]
    fn echo_and_fail() -> (PathBuf, Vec<String>) {
        (
            "sh".into(),
            ["-c", "echo boom >&2; exit 3"].map(String::from).to_vec(),
        )
    }

    #[test]
    fn a_hung_call_is_killed_at_the_deadline() {
        let (program, args) = sleeper();
        let started = Instant::now();
        let err = run_bounded(&program, &args, Duration::from_millis(300)).unwrap_err();
        assert!(err.starts_with("timed out after"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "waited for the child"
        );
    }

    #[test]
    fn a_failing_call_reports_its_status_and_herdrs_own_error_message() {
        let (program, args) = echo_and_fail();
        let err = run_bounded(&program, &args, CALL_TIMEOUT).unwrap_err();
        assert!(err.contains("boom") && err.contains('3'), "{err}");
        let json = r#"{"error":{"code":"pane_not_found","message":"pane w1:p1 not found"}}"#;
        assert_eq!(explain("exit 1", json), "pane w1:p1 not found");
        assert_eq!(
            explain("ssh exit 2", "plain text\n"),
            "ssh exit 2: plain text"
        );
        let missing = run_bounded(Path::new("herdr-does-not-exist"), &[], CALL_TIMEOUT);
        assert!(missing.unwrap_err().contains("herdr-does-not-exist"));
    }
}
