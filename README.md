# Herdr Project Picker

A standalone Rust plugin for [Herdr](https://herdr.dev): a centered popup with
a searchable project list and one tab per Herdr machine. Pick a project and it
opens as a **new workspace** in Herdr's sidebar, with the shell already in that
folder.

**Status: working local build for Herdr 0.9.3+.** Parsing of the machine
catalog and workspace snapshots was checked against a live Herdr (read-only);
the popup, keybinding and workspace creation still need a manual live check,
listed in [Validation](#validation).

## What it does

Bind it to a key (suggested `prefix+f`). The popup shows:

```
 Current  Mini  Orgo  MacBook
 › query
 agents-app  C:\projects\agents-app        <- highlighted
 herdr-project-picker
 orchestrator
```

Herdr draws the popup's frame and title in its own theme. The inside uses only
plain terminal attributes (bold, dim, reverse), so it follows the same palette
as your panes; Herdr does not give plugins its theme colors, and the plugin
copies none.

- **Type** to filter, **Up/Down** (or Ctrl-N/Ctrl-P) to choose, **Tab /
  Shift+Tab** to switch machine, **Enter** to open, **Escape** (or Ctrl-C) to
  cancel from any state. Cancelling touches nothing.
- **Current** is the machine running Herdr's server for this window. Its
  projects are the folders under your configured `roots`/`projects` plus the
  folders of the workspaces Herdr already has open (a worktree's checkout, or
  the panes' working directories; never a foreground process's directory).
- The other tabs are the **enabled saved machines** from Herdr's catalog, in
  Herdr's order. A machine's tab loads when you first open it (nothing polls)
  and lists only the folders of workspaces open on that machine, read through
  Herdr's own `--machine` bridge. Loading, unavailable (with the reason) and
  empty states are shown; there is never a silent fallback to the local list.
- **Enter on Current** creates a new workspace named after the project, with
  its shell in the project folder, and switches to it. It always creates a new
  one, even when the project is already open.
- **Enter on another machine** creates the workspace on that machine and tells
  you so. It does **not** switch the window: Herdr offers no way for a plugin
  to do that, so select it in the Herdr sidebar.
- The plugin never types into, restarts or closes an existing pane; it only
  asks Herdr to create a new workspace. Remote workspaces are created
  unfocused, because focusing is server-wide and would move every other client
  attached there.

## Limits you should know about

- **The plugin runs on the server of the machine selected when you press the
  key, and must be installed there.** Herdr does not copy plugins or their
  config to SSH machines, and this plugin does not install anything remotely.
- The machine list is the **catalog of that server**, not of the client you are
  typing in. If you invoke it while a remote machine is selected, you see that
  server's saved machines. Herdr gives plugins no profile id or name for the
  machine they run on, so it is shown as the synthetic **Current** tab; the
  persisted `selected` flag of the catalog is ignored because it does not
  describe your window.
- Project folders on other machines are the ones Herdr reports for open
  workspaces. The plugin does not scan remote disks, and your `roots` apply to
  the current machine only.
- Everything is manual: there is no automatic popup for new splits (Herdr's
  `pane.created` event cannot distinguish a split from a new tab's first pane;
  see [docs/INTEGRATION.md](docs/INTEGRATION.md)).

## Requirements

- Herdr 0.9.3 or newer on every machine you use it on (popup plugin panes;
  `--machine` routing on the machine you invoke it from).
- Rust 1.85+ with `cargo` to build (the locked dependencies need it); no other tools.
- Windows, macOS and Linux are declared in the manifest; the manifest commands
  are the same on all three (Herdr/Windows adds `.exe`).

## Install

On each machine whose Herdr server should offer the picker:

```sh
git clone <repository-url> herdr-project-picker
cd herdr-project-picker
cargo build --release --locked --target-dir target
herdr plugin link "$(pwd)"
```

(`herdr plugin install <repository-url>` runs the same build for you.
`--target-dir target` keeps the binary where the manifest looks for it even if
`CARGO_TARGET_DIR` is set. A binary put on `PATH` by `cargo install` is not
used; the manifest runs `target/release/herdr-project-picker`.)

Then bind the key. `bind-key` appends one `[[keys.command]]` entry to your
client config after backing it up, and refuses to touch a key that is already
used or a file that is not valid TOML:

```sh
target/release/herdr-project-picker bind-key --config /path/to/herdr/config.toml
# default key prefix+f; --key prefix+g to choose another
```

The client config is `%APPDATA%\herdr\config.toml` on Windows and
`~/.config/herdr/config.toml` elsewhere (`XDG_CONFIG_HOME` and
`HERDR_CONFIG_PATH` override these in the Herdr source this was checked
against); `herdr config check` validates it. The entry added is:

```toml
[[keys.command]]
key = "prefix+f"
type = "plugin_action"
command = "local.project-picker.pick"
description = "Open project"
```

Keybindings are client-local: reload the client config (or restart the client)
afterwards.

## Configure

Optional `config.toml` in the plugin config directory
(`herdr plugin config-dir local.project-picker`), see
[config.example.toml](config.example.toml):

```toml
roots = ["~/projects"]   # each subfolder is a project; absolute or ~/...
projects = []            # folders that are projects themselves
```

Without any config the picker still lists the folders of your open Herdr
workspaces. `HERDR_PROJECT_PICKER_ROOTS` (a PATH-style list) overrides `roots`.
Paths must be absolute because Herdr resolves them in its own process;
relative entries are rejected with an error.

## Validation

None of this installs, links or configures anything.

1. Scoped tests: parsing, filtering, machine cycling, routing and ID
   isolation, timeouts, sanitization, config preservation, manifest:
   ```sh
   cargo test --locked
   ```
2. Read-only checks against a running Herdr (catalog and snapshots; creates
   nothing). Add `HERDR_PICKER_LIVE_REMOTE=1` to also read
   the saved machines through Herdr's non-interactive SSH bridge:
   ```sh
   cargo test --locked live -- --ignored --nocapture
   ```
3. See the popup on its own, printing the chosen path instead of opening it:
   ```sh
   target/release/herdr-project-picker pick --print
   ```
   (Runs `herdr` from `PATH` or `HERDR_BIN_PATH` for the tabs.)

**Needs a manual live Herdr test** (not done by this repo): the keybinding
opening the popup centered; Escape closing it with the shell untouched; Enter
on Current creating a workspace in the project and switching to it; Enter on a
saved machine creating the workspace there and the sidebar finding it; a
disconnected machine showing its error; the same on Windows and on a remote
machine that has the plugin installed.
