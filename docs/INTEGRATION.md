# Herdr integration notes

What was checked against Herdr **0.9.3**, what the plugin does because of it,
and what remains unproven. Sources: the stable docs
(`plugins.mdx`, `cli-reference.mdx`, `socket-api.mdx` at tag `v0.9.3`) and a
read-only reading of the `v0.9.3` source. Nothing was run in a live Herdr.

## 1. Triggering only for new splits

**Blocker: `pane.created` carries no origin.** The event payload is
`{"event":"pane_created","data":{"type":"pane_created","pane":<PaneInfo>}}`
and `PaneInfo` (`src/api/schema/panes.rs:449`) has no field saying how the pane
came to exist. Who emits it (all `v0.9.3`):

| Source | Emits `pane.created` | Where |
| --- | --- | --- |
| Split, from the keybinding **and** from `herdr pane split` | yes | `src/app/api/panes.rs:129`; the keybinding sends the same `pane.split` request with `focus: true` (`src/client/shell/actions.rs:1037`) |
| New tab, new workspace | yes, after `tab.created`, for the tab's first pane | `src/app/creation.rs:249-275` |
| `layout.apply` | yes, for **every** pane of the new tab | `src/app/api/layouts.rs:195-210` |
| Plugin panes (split, tab, zoomed, overlay; not popups) | yes | `src/app/api/plugins/panes.rs:311` |
| Worktree create/open | yes | docs, `socket-api.mdx` |
| `pane.move` | no (documented) | `socket-api.mdx` |
| Session restore | no emit site found (source reading only) | not live-verified |

Event hooks all run with `invocation_source = "api"`, and hooks are separate
processes started per event, so ordering between a `tab.created` hook and a
`pane.created` hook is not guaranteed. A plugin therefore cannot prove a pane is
a user split. What the hook can see is used as a heuristic
(`src/trigger.rs`), and the hook is inert unless `auto_trigger = true`:

1. Only `pane_created` events are considered.
2. Skip panes with a `label` (plugin panes get their manifest title as a manual
   label; ordinary new panes have none).
3. Skip panes that already have an `agent`.
4. Require `focused: true`. The keybinding split focuses the new pane, while
   `herdr pane split` leaves focus alone unless `--focus` is passed, so most
   automated splits are skipped. The picker's own splits rely on this: it
   creates them with `--no-focus`, otherwise its project split would look like
   a user split and reopen the picker.
5. Skip the first pane of a tab: ask `herdr pane list --workspace <id>` and
   skip unless the tab has a pane with a **lower number**. Comparing numbers
   (instead of counting panes) keeps a new tab's first pane out even if it was
   split before the hook ran.

Known gaps, all in the direction of false positives: an agent or script that
splits with `--focus`; a `layout.apply` tab whose focused pane is not its
first; a pane moved in with a lower number. Known gaps in the direction of
false negatives: a first pane that was closed leaves its sibling looking
initial. This is why it ships disabled.

**Smallest upstream extension to make it reliable:** add an origin to the
`pane.created` payload, for example `"origin": "split" | "tab_root" |
"workspace_root" | "layout" | "plugin" | "worktree"` plus, for splits,
`"source_pane_id"` and whether the request came from the keybinding. With that
the filter is one comparison and needs no `pane list` call.

## 2. Safe handoff to the new shell

Herdr has no method that changes an existing pane's directory
(`src/api/schema.rs` method list). The only other route is typing `cd` into the
shell with `pane run` / `send-text`, which the plugin refuses to do: quoting is
shell-specific (bash, zsh, fish, PowerShell, cmd) and the shell may be busy.

What it does instead, for both the manual action and the automatic trigger:

- `herdr pane split --pane <pane> --direction <right|down> --cwd <project>
  --no-focus`, then `herdr pane focus --direction <same> --pane <pane>`. The
  shell is launched *in* the project, so there is no handoff to a running
  shell at all, and no existing pane is typed into, restarted or closed.
  `--cwd` is passed to Herdr's server process, so configured roots and
  projects must be absolute (`~` is expanded); relative ones are rejected.
- The split is created unfocused so the event hook cannot mistake it for a
  user split (see section 1). Herdr 0.9.3 has no way to focus a pane by id
  (`pane focus` takes only a direction), so focus moves to the split's
  neighbor in the split direction. If that call fails the project is still
  open; only the focus is left where it was.
- **Escape / Ctrl-C** exit without any Herdr call.

**Not possible safely in 0.9.3: replacing the new split's own shell.** With the
automatic trigger the new split keeps its (empty) shell and the project opens
in a further split beside it; the user closes the extra pane. An earlier draft
closed the new pane after checking that its shell was the only foreground
process, but that does not prove idleness: a shell running a builtin such as
`read` is its own foreground process, so a busy shell would have been closed.
No API reports "at a prompt".

**Smallest upstream extensions for a true in-place handoff:** a method to
(re)start the shell of an idle pane in a directory, such as
`pane respawn <pane> --cwd PATH`, or a `cwd` that a `pane.created` hook can
supply before the shell starts; plus focusing a pane by id.

## 3. Popup and rendering

Plugin v1 has no native non-terminal UI, so the dialog is a Rust terminal
program (crossterm, alternate screen, blocking reads: no polling) in a
`placement = "popup"` pane. Popups are session-modal, have no pane id, emit no
pane lifecycle events and receive all input including Escape
(`plugins.mdx`). The popup is opened with `herdr plugin pane open` through
`HERDR_BIN_PATH`. The target pane is passed as `HERDR_PICKER_PANE` because a
popup process gets no `HERDR_PANE_ID`. Directory names are printed with control
characters escaped, so a name holding terminal escape codes cannot rewrite the
popup; the real path is still what is handed to Herdr. Opening can
fail with `ui_busy` when another Herdr modal is active; the error is then
only in the plugin command log (`herdr plugin log list`).

## 4. Checked here vs. unproven

Checked on this machine (Windows, without Herdr involvement):

- `cargo test`: unit tests for the filter, config/path handling, handoff and
  search, plus a manifest contract test.
- `cargo clippy --all-targets` and `cargo fmt --check`.
- The release binary against a fake `HERDR_BIN_PATH` script: the exact Herdr
  CLI arguments for the manual action and the auto trigger (a focused split
  opens the popup; an unfocused split, a new tab's first pane and a
  `tab_created` event do not; the trigger is inert without `auto_trigger`),
  and a relative root being rejected. The project split's own arguments
  (`--no-focus`, then `pane focus --direction`) are covered by unit tests
  with a mock only, since the popup UI needs a terminal.
- The popup UI drawing its list in a real console (`pick --print`).

**Unproven, needs a live Herdr:**

1. `herdr plugin link` accepting the manifest (it is only parsed by the test).
2. The popup opening and sizing, Escape/Enter inside it, and the keybinding.
3. On Windows, whether Herdr's action, event and pane launchers resolve the
   extension-less `target/release/herdr-project-picker` to the `.exe`.
4. That a keybinding split really arrives with `focused: true`, that session
   restore emits no `pane.created`, and the heuristic's false positives.
5. `pane split --no-focus` and `pane focus --direction` called from inside an
   open popup, including that the focus move lands on the new split and that
   the unfocused split's `pane.created` event reports `focused: false`.
6. `--env` reaching the popup process.
