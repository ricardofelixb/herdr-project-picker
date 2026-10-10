//! Talks to Herdr through its CLI (`HERDR_BIN_PATH`) to open the popup and to
//! open the chosen project in a new pane.
//!
//! Herdr cannot change the directory of an existing pane, and typing `cd`
//! into one is unsafe (shell-specific quoting, busy shells). A project is
//! therefore only ever opened by launching a *new* pane with `--cwd`.
//! Existing panes are never typed into, restarted or closed: no `pane run`,
//! `send-text`, `send-keys` or `close` is used anywhere.
//!
//! Pane and workspace ids belong to one Herdr server. Every command for a
//! saved machine is routed with `--machine <opaque profile id>` and uses only
//! ids that came from that machine's own snapshot.

use crate::config::Direction;
use crate::machine::Machine;
use crate::snapshot::{is_absolute, Destination, Row};
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

pub const PICKER_ENTRYPOINT: &str = "picker";
pub const ENV_PANE: &str = "HERDR_PICKER_PANE";

/// How long one Herdr call may take. Remote calls go through Herdr's own
/// non-interactive SSH bridge, which fails rather than prompting.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(20);

pub trait Herdr {
    /// Runs `herdr <args>`; returns stdout, or the failure text.
    fn run(&self, args: &[String]) -> Result<String, String>;
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
    Err(explain(
        &status.to_string(),
        &String::from_utf8_lossy(&stderr),
    ))
}

/// Herdr reports failures as `{"error":{"message":...}}` on stderr.
fn explain(status: &str, stderr: &str) -> String {
    serde_json::from_str::<Value>(stderr.trim())
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| format!("herdr {status}: {}", stderr.trim()))
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

/// Opens the picker popup, remembering the pane the project opens beside.
pub fn open_popup(herdr: &dyn Herdr, plugin_id: &str, pane: &str) -> Result<(), String> {
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
                "--placement",
                "popup",
                "--width",
                "70%",
                "--height",
                "60%",
                "--env",
                &format!("{ENV_PANE}={pane}"),
            ],
        ))
        .map(drop)
}

/// What opening a row touches, once it is known to belong to the machine it
/// is opened on.
enum Plan<'a> {
    /// Beside the pane the popup was opened from.
    Local { pane: &'a str },
    /// Beside the project's own workspace pane, on the saved machine.
    Remote { id: &'a str, dest: &'a Destination },
}

/// Refuses to mix machines: a row loaded from one machine carries that
/// machine's ids and is never opened on another, and local ids never reach a
/// remote command.
fn plan<'a>(
    machine: &'a Machine,
    row: &'a Row,
    invoking_pane: &'a str,
) -> Result<Plan<'a>, String> {
    if !is_absolute(&row.path) {
        return Err(format!("{} is not an absolute path", row.path));
    }
    match (machine.id.as_deref(), row.dest.as_ref()) {
        (None, dest) if dest.is_none_or(|dest| dest.machine.is_none()) => Ok(Plan::Local {
            pane: invoking_pane,
        }),
        (Some(id), Some(dest)) if dest.machine.as_deref() == Some(id) => {
            Ok(Plan::Remote { id, dest })
        }
        _ => Err(format!(
            "{} does not belong to {}; nothing was opened",
            row.path, machine.label
        )),
    }
}

/// Opens `row` in a new pane. `Ok(None)` means done and nothing to tell the
/// user (the pane is in front of them); `Ok(Some(text))` is what to tell
/// them, because the new pane is on another machine.
///
/// The split is created unfocused: a focused, unlabeled pane with an older
/// sibling looks like a user split, so nothing else may react to ours. The
/// 0.9.3 CLI focuses only by direction (the socket's `pane.focus` by id has no
/// CLI wrapper), so a local pane is then focused by moving from its source.
/// Remote machines are never focused: focus is server-wide and would move
/// every other client attached to that server.
pub fn open_project(
    herdr: &dyn Herdr,
    machine: &Machine,
    row: &Row,
    invoking_pane: &str,
    direction: Direction,
) -> Result<Option<String>, String> {
    let direction_arg = direction.as_str();
    match plan(machine, row, invoking_pane)? {
        Plan::Local { pane } => {
            split(herdr, None, pane, direction_arg, &row.path)?;
            // Cosmetic, hence best effort: the project is already open.
            let _ = herdr.run(&routed(
                None,
                &[
                    "pane",
                    "focus",
                    "--direction",
                    direction_arg,
                    "--pane",
                    pane,
                ],
            ));
            Ok(None)
        }
        Plan::Remote { id, dest } => {
            check_destination(herdr, id, dest)?;
            split(herdr, Some(id), &dest.pane_id, direction_arg, &row.path)?;
            Ok(Some(format!(
                "Opened {} in a new pane on {}, workspace {} ({}).\n\
                 Herdr cannot switch this window to another machine:\n\
                 select {} in the Herdr sidebar to see it.",
                row.name, machine.label, dest.workspace_label, dest.workspace_id, machine.label
            )))
        }
    }
}

/// A new, unfocused pane beside `pane` with its shell in `cwd`.
fn split(
    herdr: &dyn Herdr,
    machine: Option<&str>,
    pane: &str,
    direction: &str,
    cwd: &str,
) -> Result<String, String> {
    let args = [
        "pane",
        "split",
        "--pane",
        pane,
        "--direction",
        direction,
        "--cwd",
        cwd,
        "--no-focus",
    ];
    herdr.run(&routed(machine, &args))
}

/// Fails closed unless the destination pane still exists on that machine and
/// still belongs to the workspace the project was found in.
pub(crate) fn check_destination(
    herdr: &dyn Herdr,
    id: &str,
    dest: &Destination,
) -> Result<(), String> {
    let stale = |why: &str| {
        format!(
            "workspace {} is no longer at the saved destination ({why}); reopen the picker",
            dest.workspace_label
        )
    };
    let reply = herdr
        .run(&routed(Some(id), &["pane", "get", &dest.pane_id]))
        .map_err(|err| stale(&err))?;
    let reply: Value = serde_json::from_str(&reply).map_err(|err| stale(&err.to_string()))?;
    if let Some(message) = reply["error"]["message"].as_str() {
        return Err(stale(message));
    }
    let pane = &reply["result"]["pane"];
    let same = pane["pane_id"].as_str() == Some(dest.pane_id.as_str())
        && pane["workspace_id"].as_str() == Some(dest.workspace_id.as_str());
    if same {
        Ok(())
    } else {
        Err(stale("pane moved to another workspace"))
    }
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
    }
}

#[cfg(test)]
mod tests {
    use super::mock::Mock;
    use super::*;

    fn dest(machine: Option<&str>, pane: &str, workspace: &str) -> Destination {
        Destination {
            machine: machine.map(str::to_string),
            pane_id: pane.into(),
            workspace_id: workspace.into(),
            workspace_label: "api".into(),
        }
    }

    fn remote(id: &str) -> Machine {
        Machine {
            id: Some(id.into()),
            label: "Orgo".into(),
        }
    }

    fn pane_reply(pane: &str, workspace: &str) -> String {
        format!(
            r#"{{"id":"x","result":{{"pane":{{"pane_id":"{pane}","workspace_id":"{workspace}"}}}}}}"#
        )
    }

    /// Nothing is ever typed into, restarted, closed or (remotely) focused.
    fn only_new_panes(mock: &Mock) {
        for line in mock.joined() {
            for forbidden in [
                "pane run",
                "pane send-text",
                "pane send-keys",
                "pane close",
                " --focus",
            ] {
                assert!(!line.contains(forbidden), "unsafe: {line}");
            }
        }
    }

    #[test]
    fn popup_is_opened_with_the_target_pane_but_no_shell() {
        let mock = Mock::default();
        open_popup(&mock, "local.project-picker", "w1:p3").unwrap();
        let call = mock.joined().remove(0);
        assert!(
            call.starts_with("plugin pane open --plugin local.project-picker --entrypoint picker")
        );
        assert!(call.contains("--placement popup"));
        assert!(call.contains("--env HERDR_PICKER_PANE=w1:p3"));
        assert!(!call.contains("--machine"));
    }

    // The split must not be created focused: it would look like a user split.
    #[test]
    fn local_project_opens_unfocused_beside_the_invoking_pane_then_focus_moves() {
        let mock = Mock::default();
        let row = Row::new("/work/my project", None);
        let done = open_project(&mock, &Machine::current(), &row, "w1:p2", Direction::Down);
        assert_eq!(done, Ok(None));
        assert_eq!(
            mock.joined(),
            [
                "pane split --pane w1:p2 --direction down --cwd /work/my project --no-focus",
                "pane focus --direction down --pane w1:p2",
            ]
        );
        only_new_panes(&mock);
    }

    #[test]
    fn a_failed_local_focus_move_does_not_fail_the_handoff() {
        let mock = Mock::default().reply("pane focus", Err("no neighbor"));
        let row = Row::new("/work/a", None);
        assert_eq!(
            open_project(&mock, &Machine::current(), &row, "w1:p2", Direction::Right),
            Ok(None)
        );
    }

    #[test]
    fn a_failed_split_is_reported_and_nothing_else_runs() {
        let mock = Mock::default().reply("pane split", Err("no space"));
        let row = Row::new("/work/a", None);
        let done = open_project(&mock, &Machine::current(), &row, "w1:p2", Direction::Right);
        assert_eq!(done, Err("no space".to_string()));
        assert_eq!(mock.calls().len(), 1);
    }

    #[test]
    fn remote_project_is_routed_by_profile_id_to_its_own_destination_pane() {
        let mock =
            Mock::default().reply("--machine prof-b pane get", Ok(&pane_reply("w1:p1", "w1")));
        let row = Row::new("/srv/api", Some(dest(Some("prof-b"), "w1:p1", "w1")));
        // The popup was opened from a LOCAL pane with an unrelated id.
        let told = open_project(&mock, &remote("prof-b"), &row, "w9:p9", Direction::Right).unwrap();
        assert_eq!(
            mock.joined(),
            [
                "--machine prof-b pane get w1:p1",
                "--machine prof-b pane split --pane w1:p1 --direction right --cwd /srv/api --no-focus",
            ]
        );
        assert!(
            !mock.joined().join(" ").contains("w9:p9"),
            "a local id reached a remote command"
        );
        only_new_panes(&mock);
        let told = told.expect("remote results are reported");
        for part in ["api", "Orgo", "w1", "sidebar"] {
            assert!(told.contains(part), "{part}: {told}");
        }
    }

    // Two servers may both have a `w1:p1`; each goes only to its own profile.
    #[test]
    fn identical_ids_on_two_machines_never_cross() {
        let mock = Mock::default().reply("--machine", Ok(&pane_reply("w1:p1", "w1")));
        for id in ["prof-a", "prof-b"] {
            let row = Row::new("/srv/api", Some(dest(Some(id), "w1:p1", "w1")));
            open_project(&mock, &remote(id), &row, "w9:p9", Direction::Right).unwrap();
        }
        let calls = mock.calls();
        assert_eq!(calls.len(), 4);
        assert_eq!(&calls[0][..2], ["--machine", "prof-a"]);
        assert_eq!(&calls[1][..2], ["--machine", "prof-a"]);
        assert_eq!(&calls[2][..2], ["--machine", "prof-b"]);
        assert_eq!(&calls[3][..2], ["--machine", "prof-b"]);
    }

    #[test]
    fn a_row_is_never_opened_on_a_machine_it_was_not_loaded_from() {
        let mock = Mock::default();
        let from_a = Row::new("/srv/api", Some(dest(Some("prof-a"), "w1:p1", "w1")));
        let local_id = Row::new("/work/api", Some(dest(None, "w1:p1", "w1")));
        let configured = Row::new("/work/api", None);
        let cases = [
            (remote("prof-b"), &from_a),
            (Machine::current(), &from_a),
            (remote("prof-a"), &local_id),
            (remote("prof-a"), &configured),
        ];
        for (machine, row) in cases {
            let err = open_project(&mock, &machine, row, "w9:p9", Direction::Right).unwrap_err();
            assert!(err.contains("does not belong"), "{err}");
        }
        assert!(mock.calls().is_empty(), "{:?}", mock.joined());
    }

    #[test]
    fn a_stale_destination_fails_closed_without_creating_a_pane() {
        let row = Row::new("/srv/api", Some(dest(Some("prof-b"), "w1:p1", "w1")));
        let gone = Mock::default().reply("--machine prof-b pane get", Err("pane w1:p1 not found"));
        let moved =
            Mock::default().reply("--machine prof-b pane get", Ok(&pane_reply("w1:p1", "w7")));
        let refused = Mock::default().reply(
            "--machine prof-b pane get",
            Ok(r#"{"error":{"code":"pane_not_found","message":"pane w1:p1 not found"},"id":"x"}"#),
        );
        let garbled = Mock::default().reply("--machine prof-b pane get", Ok("not json"));
        for mock in [gone, moved, refused, garbled] {
            let err = open_project(&mock, &remote("prof-b"), &row, "w9:p9", Direction::Right)
                .unwrap_err();
            assert!(err.contains("no longer at the saved destination"), "{err}");
            assert_eq!(mock.calls().len(), 1, "{:?}", mock.joined());
        }
    }

    #[test]
    fn an_unavailable_machine_is_an_error_not_a_local_fallback() {
        let mock = Mock::default().reply("--machine", Err("ssh: connection refused"));
        let row = Row::new("/srv/api", Some(dest(Some("prof-b"), "w1:p1", "w1")));
        let err =
            open_project(&mock, &remote("prof-b"), &row, "w9:p9", Direction::Right).unwrap_err();
        assert!(err.contains("connection refused"), "{err}");
        assert!(mock
            .joined()
            .iter()
            .all(|line| line.starts_with("--machine prof-b ")));
    }

    #[test]
    fn a_failed_remote_split_is_reported() {
        let mock = Mock::default()
            .reply("--machine prof-b pane get", Ok(&pane_reply("w1:p1", "w1")))
            .reply("--machine prof-b pane split", Err("no space"));
        let row = Row::new("/srv/api", Some(dest(Some("prof-b"), "w1:p1", "w1")));
        let done = open_project(&mock, &remote("prof-b"), &row, "w9:p9", Direction::Right);
        assert_eq!(done, Err("no space".to_string()));
    }

    #[test]
    fn relative_paths_never_reach_herdr() {
        let mock = Mock::default();
        for row in [
            Row::new("projects/api", None),
            Row::new("~/api", Some(dest(Some("prof-b"), "w1:p1", "w1"))),
        ] {
            for machine in [Machine::current(), remote("prof-b")] {
                assert!(open_project(&mock, &machine, &row, "w1:p1", Direction::Right).is_err());
            }
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
            explain("exit 2", "plain text\n"),
            "herdr exit 2: plain text"
        );
        let missing = run_bounded(Path::new("herdr-does-not-exist"), &[], CALL_TIMEOUT);
        assert!(missing.unwrap_err().contains("herdr-does-not-exist"));
    }
}
