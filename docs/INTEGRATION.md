# Herdr integration notes

What was checked against Herdr, what the plugin does because of it, and what
remains unproven. Sources: the stable `0.9.3` docs (`plugins.mdx`,
`cli-reference.mdx`, `socket-api.mdx` at tag `v0.9.3`), a read-only reading of
the `0.9.3` source and of the installed `0.9.3-voice-canary.20261006.1` build
(a community build on upstream commit `7b116c05`), and read-only queries
against a live Herdr (section 7).

## 1. Machines

Herdr's saved SSH machines are client state: `herdr machine list --json` prints
an array of `{id, label, target, session, enabled, selected}`; `id` is an opaque
profile id (`src/cli/machine.rs`). The plugin offers:

- **Current**: a synthetic tab for the machine running the plugin. Herdr gives
  plugin processes `HERDR_SOCKET_PATH`, `HERDR_BIN_PATH` and a context with
  workspace/tab/pane ids, but no machine, endpoint or client id
  (`src/api/schema/plugins.rs:364-395`), so there is nothing truthful to call it
  other than "Current". A saved profile pointing at the same host would be a
  guess, and hostnames are never compared.
- **Saved machines**: the `enabled` rows, in the order Herdr lists them. The
  `id` is the only routing identity (`herdr --machine <id> ...`); labels may
  repeat and are display text only. The `target` (SSH host) is never read, and
  `selected` is persisted state, not the invoking window's live selection
  (`src/client/endpoint/catalog.rs:103-123`), so it is ignored.

A plugin runs on the server of the selected machine and sees that server's own
catalog (`state_dir/client/endpoints.json` there), not the catalog of the client
that sent the keypress. Herdr does not copy plugins or config to SSH machines
([Connecting machines](https://herdr.dev/docs/connecting-machines/#settings-and-automation)),
so the plugin must be installed on each server where it should be offered, and
nothing here installs it remotely or merges catalogs.

## 2. Project metadata

`herdr api snapshot` (also routable with `--machine`; `src/cli/target.rs`
allows `api snapshot`) replies `{"result":{"snapshot":{workspaces, panes, tabs,
...}}}`. Only identifiers, labels and directories are read:

- A workspace's `worktree.checkout_path` when it has one, otherwise the `cwd`
  of its panes; a pane's `foreground_cwd` is **never** used (it is wherever a
  tool happens to run, not the project).
- Folders nested inside another folder of the same workspace fold into the
  outer one, except the worktree checkout, which Herdr names as the root and
  which a pane sitting in one of its parent folders never hides; separate
  workspaces keep their own folders.
- Repeats across workspaces collapse deterministically (first workspace wins;
  Windows-style paths compare without case or separator differences); the
  list is sorted by name.
- Each row keeps the pane id (preferring the focused pane that is at the
  folder), workspace id and workspace label **of the machine it came from**,
  together with that machine's profile id.
- Titles, terminal titles and agent data in the snapshot are not copied.
- Paths are strings: a Unix path from a remote machine is absolute even when
  the plugin runs on Windows (`Path::is_absolute` would disagree).

`herdr worktree list` is not called: the snapshot already carries the worktree
checkout of every open workspace. Snapshots describe open sessions,
not every project on disk, which is why the current machine also reads the
configured `roots`; remote disks are not scanned.

## 3. Opening a project

A project is only ever opened by asking Herdr for a **new** workspace whose
shell starts in the project: `herdr [--machine <id>] workspace create --cwd
<project> --label <folder name> --focus|--no-focus`. Existing panes are never
typed into, restarted or closed (`pane run`, `send-text`, `send-keys`, `pane
close` and `pane split` are never used, and a test asserts it). It always
creates a new workspace, even when the project is already open; that was the
user's choice. Herdr's parser takes the argument after `--cwd`/`--label`
verbatim (`src/cli/workspace.rs`), so a folder named like an option stays a
value.

- **Current**: created with `--focus`, so the window switches to it.
- **Saved machine**: routed with `--machine <opaque profile id>` and created
  with `--no-focus`. A row only opens on the machine it was read from, and a
  configured/local row is refused on a saved machine. Remote machines are never
  focused: `pane.focus` and `workspace.focus` are server-wide and would move
  every other client attached to that server
  (`public_api_focus_replaces_every_client_shell_projection`, installed source).
  The popup says which machine got the workspace and to select it in the
  sidebar.
- **No client switch.** Switching the invoking window to another machine is an
  internal client action (`ActivateEndpoint`,
  `src/client/shell/endpoint_navigation.rs:228-296`); CLI `--machine` routing is
  command-scoped, "never the runtime or TUI" (`src/cli/target.rs:7-9`). That is
  the smallest upstream gap: a public request that switches the calling client's
  machine and/or reports the caller's current profile id in the plugin context.
- Paths are passed to Herdr's server process, so they must be absolute
  (checked on the plugin side, platform-independently).
- Escape exits without any Herdr call.

`--machine` calls run Herdr's own non-interactive SSH bridge: it never installs,
starts or restarts a server, and authentication, unreachable machines and
incompatible versions fail without falling back to Local and without automatic
retry (`cli-reference.mdx`). Each call here is bounded (20 s), after which the
process is killed and the tab shows "timed out"; revisiting a failed tab is the
only retry.

## 4. Popup and rendering

Plugin v1 has no native non-terminal UI, so the dialog is a Rust terminal
program (crossterm, alternate screen) in a `placement = "popup"` pane. Popups
are session-modal, have no pane id, emit no pane lifecycle events and receive
all input including Escape (`plugins.mdx`). The popup is opened with `herdr
plugin pane open` through `HERDR_BIN_PATH`; its size (56x16 cells, shrunk to fit)
and title come from the manifest. Opening can
fail with `ui_busy` when another Herdr modal is active; the error is then only
in the plugin command log (`herdr plugin log list`). Herdr draws the popup
border and title in its theme. Herdr keeps its theme palette in the client and
gives panes, popups included, only the host terminal's palette
(`host_terminal_theme`, `src/app/api/plugins/panes.rs`), so the plugin uses the
terminal's default colors plus bold/dim/reverse and copies no theme colors.

Threads, no polling: one thread blocks reading the terminal, a worker per
machine tab loads its projects, a worker performs the open, and the UI loop
blocks on a single channel they all send to. A slow or hung machine therefore
only delays its own tab, and Escape works at any time (while an open is in
flight it stops waiting, but Herdr may still finish the request). Every string drawn, including remote labels, paths and error text, is
printed with control characters escaped, so a hostile directory name cannot
rewrite the screen.

## 5. Why there is no automatic trigger

An earlier version could open the picker for new splits. It was removed: the
manual key is the product, and the hook cost a process per pane for a guess.

**Blocker: `pane.created` carries no origin.** The payload is
`{"event":"pane_created","data":{"type":"pane_created","pane":<PaneInfo>}}` and
`PaneInfo` (`src/api/schema/panes.rs`) has no field saying how the pane came to
exist. Who emits it (`0.9.3`):

| Source | Emits `pane.created` | Where |
| --- | --- | --- |
| Split, from the keybinding **and** from `herdr pane split` | yes | `src/app/api/panes.rs:129`; the keybinding sends the same `pane.split` request with `focus: true` (`src/client/shell/actions.rs:1037`) |
| New tab, new workspace | yes, for the tab's first pane | `src/app/creation.rs:249-275` |
| `layout.apply` | yes, for **every** pane of the new tab | `src/app/api/layouts.rs:195-210` |
| Plugin panes (split, tab, zoomed, overlay; not popups) | yes | `src/app/api/plugins/panes.rs:311` |
| Worktree create/open | yes | `socket-api.mdx` |
| `pane.move` | no (documented) | `socket-api.mdx` |

Hooks are separate processes, so ordering against a `tab.created` hook is not
guaranteed either. **Smallest upstream extension:** an origin in the payload,
for example `"origin": "split" | "tab_root" | ...` plus the source pane id.
Replacing the new split's own shell is also not possible safely: no API reports
"at a prompt" (a shell running a builtin like `read` is its own foreground
process), so closing the pane after a foreground check could kill live work.
A future `pane respawn <pane> --cwd PATH`, or a `cwd` a `pane.created` hook can
supply before the shell starts, would allow an in-place hand-off.

## 6. Portability

- Manifest commands are argv arrays, `target/release/herdr-project-picker`,
  relative to the plugin folder, no extension and no shell wrapper. Herdr joins
  it onto the plugin root (`program_for_cwd`); action commands are spawned by
  Rust, which appends `.exe`, and the Windows pane launcher tries `PATHEXT`
  extensions (`vendor/portable-pty/src/cmdbuilder.rs`, `search_path`, which
  swaps the extension, so the binary name must not contain a dot; a test
  guards it). Source-read only: not run in a live Windows popup.
- The build passes `--target-dir target` so the binary is where the commands
  look even if `CARGO_TARGET_DIR` is set. `cargo install` is not used by the
  plugin.
- `min_herdr_version = "0.9.3"`; `--machine` and `machine list --json` are in
  the `0.9.3` CLI docs.
- The plugin id `local.project-picker` is stable; `bind-key` derives the
  qualified action id from the manifest, so renaming it to an owner-qualified id
  before publishing needs no code change.

## 7. Checked here vs. unproven

Checked on this machine (Windows, Herdr `0.9.3-voice-canary.20261006.1`):

- `cargo test --locked`: unit tests for snapshot extraction and merging, machine
  catalog parsing, tab cycling/filter/load state, `--machine` routing, rows
  refused on other machines, unreachable-machine handling, bounded
  subprocess timeout, terminal-control escaping, `bind-key` config
  preservation, and manifest commands.
- `cargo test --locked live -- --ignored`: read-only against the running
  Herdr: the real `machine list --json` and `api snapshot` of the current
  machine and of every saved machine through `--machine` parse into rows with
  their own profile ids.
- `herdr config check`, and the key conflict check before binding.
- `herdr plugin list --json` shows the edited manifest (title, popup size)
  without re-linking.

Confirmed live by the user (MacBook client viewing the Windows PC's server):
`prefix+f` after a client config reload opened the popup, the extensionless
manifest commands ran on Windows, and Enter opened the project (as a split,
before the switch to workspaces).

**Unproven, needs a live Herdr:**

1. `workspace create --focus` from inside the popup on Current, and the
   compact layout at the 56x16 size.
2. `--machine <id> workspace create --no-focus` on a saved machine, and the
   sidebar finding it.
3. Tab/Shift+Tab as the terminal delivers them (Shift+Tab as `BackTab`).
4. The plugin running with a remote server selected (it must be installed
   there).
