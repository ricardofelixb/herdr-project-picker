# Herdr Project Picker

A standalone Rust plugin proof of concept for Herdr: a clean, centered project
picker when splitting a new pane. Creating tabs continues to work normally.

## Initial scope

- Herdr-hosted terminal popup, with a searchable project list.
- Configurable project roots on the current machine; no orchestrator dependency.
- Enter selects a project; Escape leaves the normal shell alone.
- Verify split-only triggering and safe shell-directory handoff before expanding
  the implementation. Document API limitations rather than disguising them.

Local development only for now; no remote repository or automatic installation.
