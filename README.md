# Herdr Project Picker

A standalone Rust plugin proof of concept for [Herdr](https://herdr.dev): a
searchable project list in a centered Herdr popup. Pick a project and it opens
in a new split, with the shell already in the project folder.

**Status: local proof of concept for Herdr 0.9.3.** Nothing here has been run
inside a live Herdr yet; see [Validation](#validation) and
[docs/INTEGRATION.md](docs/INTEGRATION.md) for what is and is not proven.

## What it does

- **Manual action** `local.project-picker.pick`: opens the popup. Type to
  filter, Up/Down (or Ctrl-N/Ctrl-P) to move, Enter to open the project in a
  new split next to the focused pane, Escape (or Ctrl-C) to cancel. Cancelling
  touches nothing.
- **Project roots are configuration**, read from the plugin's own `config.toml`
  (see [config.example.toml](config.example.toml)). There are no built-in
  paths and no dependency on any other tool.
- **Safe handoff**: the project reaches the shell only through
  `herdr pane split --cwd <project>`, which starts a *new* shell in that folder.
  The plugin never types into, restarts or closes an existing pane (`pane run`,
  `send-text`, `send-keys` and `pane close` are not used).
- **Paths must be absolute** (`~` is expanded). Herdr resolves the folder in
  its own process, so a relative path would open the wrong place; relative
  entries are rejected with an error.
- **Automatic trigger for new splits** is implemented but **off by default**
  (`auto_trigger = false`). Herdr 0.9.3 gives the `pane.created` event no
  origin, so the plugin can only guess that a pane is a user split, never
  prove it. New tabs are never intended to trigger it. When it fires, the
  new split keeps its own empty shell and the project opens in a further
  split beside it: Herdr 0.9.3 offers no safe way to replace or re-enter the
  new pane's shell. The precise blocker and the smallest upstream changes are
  in [docs/INTEGRATION.md](docs/INTEGRATION.md).

## Requirements

- Herdr 0.9.3 or newer (popup plugin panes).
- A Rust toolchain with `cargo` to build; no other system tools.

## Build

```sh
cargo build --release --locked
```

`herdr plugin install` runs this for you (`[[build]]` in the manifest);
`herdr plugin link` does not, so build first when linking a checkout. The
manifest runs `target/release/herdr-project-picker` from the plugin folder.

## Configure

Create `config.toml` in the plugin config directory
(`herdr plugin config-dir local.project-picker`):

```toml
roots = ["~/projects"]   # each subfolder is a project; absolute or ~/...
projects = []            # folders that are projects themselves
direction = "right"      # or "down"
auto_trigger = false     # see docs/INTEGRATION.md before enabling
```

`HERDR_PROJECT_PICKER_ROOTS` (a PATH-style list) overrides `roots`, which is
handy for trying the picker without any config.

Bind the action to a key in your own Herdr config, for example:

```toml
[[keys.command]]
key = "prefix+p"
type = "plugin_action"
command = "local.project-picker.pick"
description = "project picker"
```

## Validation

Nothing below needs Herdr installed, linked or configured.

1. Unit and manifest tests (trigger filtering, config and path handling, safe
   handoff, search and key handling, manifest/binary contract):
   ```sh
   cargo test
   ```
2. See and drive the popup UI on its own, printing the chosen path instead of
   calling Herdr:
   ```sh
   HERDR_PROJECT_PICKER_ROOTS=/path/to/projects target/release/herdr-project-picker pick --print
   ```

Live check, to be run by you when you want to (it registers the plugin in
Herdr's user-wide plugin list, so it is deliberately not done by this repo):

```sh
herdr plugin link /path/to/herdr-project-picker
herdr plugin action invoke local.project-picker.pick   # popup should open
# Enter: a new split opens in the project. Escape: nothing changes.
herdr plugin unlink local.project-picker               # undo
```

To try the automatic trigger, set `auto_trigger = true`, then create a split,
a new tab, a new workspace and restart Herdr; the popup should appear only for
the split. Report any other case: it is the unproven part.
