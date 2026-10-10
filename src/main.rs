//! Herdr project picker plugin.
//!
//! Subcommands, wired up in `herdr-plugin.toml`:
//! - `open`: action; opens the picker popup for a new split beside the focused pane.
//! - `pick`: popup entrypoint; the searchable list. `pick --print` just prints
//!   the chosen path, which needs no running Herdr.
//! - `on-pane-created`: event hook; opens the picker for a new split when
//!   `auto_trigger` is on (off by default, see docs/INTEGRATION.md). The
//!   project then opens in a further split; the new pane is left untouched.

mod config;
mod handoff;
mod picker;
mod trigger;

use config::{Config, CONFIG_FILE};
use handoff::{Herdr, HerdrCli};
use std::path::PathBuf;
use std::process::ExitCode;

const ENV_ROOTS: &str = "HERDR_PROJECT_PICKER_ROOTS";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let result = match args.next().as_deref() {
        Some("open") => open(),
        Some("pick") => pick(args.any(|arg| arg == "--print")),
        Some("on-pane-created") => on_pane_created(),
        _ => Err("usage: herdr-project-picker open | pick [--print] | on-pane-created".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("herdr-project-picker: {message}");
            ExitCode::FAILURE
        }
    }
}

fn env(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("{name} is not set (is this running inside Herdr?)"))
}

fn herdr() -> HerdrCli {
    HerdrCli(std::env::var_os("HERDR_BIN_PATH").map_or_else(|| "herdr".into(), PathBuf::from))
}

fn config_dir() -> Option<PathBuf> {
    std::env::var_os("HERDR_PLUGIN_CONFIG_DIR").map(PathBuf::from)
}

/// Outside Herdr there is no config directory; `HERDR_PROJECT_PICKER_ROOTS`
/// alone is enough to try the picker with `pick --print`.
fn load_config() -> Result<Config, String> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    let roots = std::env::var_os(ENV_ROOTS);
    match (config_dir(), roots) {
        (Some(dir), roots) => Config::load(&dir, roots.as_deref(), home.as_deref()),
        (None, Some(roots)) => Ok(Config {
            roots: config::roots_from_list(&roots)?,
            ..Config::default()
        }),
        (None, None) => Err(format!(
            "HERDR_PLUGIN_CONFIG_DIR is not set (is this running inside Herdr?); \
             set {ENV_ROOTS} to try the picker on its own"
        )),
    }
}

fn open() -> Result<(), String> {
    handoff::open_popup(&herdr(), &env("HERDR_PLUGIN_ID")?, &env("HERDR_PANE_ID")?)
}

fn on_pane_created() -> Result<(), String> {
    if !load_config()?.auto_trigger {
        return Ok(());
    }
    let Some(created) = trigger::created_pane(&env("HERDR_PLUGIN_EVENT_JSON")?) else {
        return Ok(());
    };
    let herdr = herdr();
    let listing = herdr.run(&[
        "pane".into(),
        "list".into(),
        "--workspace".into(),
        created.workspace_id.clone(),
    ])?;
    let panes = trigger::listed_panes(&listing).ok_or("unreadable `pane list` output")?;
    if trigger::decide(&created, &panes) == trigger::Decision::Open {
        handoff::open_popup(&herdr, &env("HERDR_PLUGIN_ID")?, &created.pane_id)?;
    }
    Ok(())
}

fn pick(print_only: bool) -> Result<(), String> {
    let pane = if print_only {
        String::new()
    } else {
        env(handoff::ENV_PANE)?
    };
    let config = match load_config() {
        Ok(config) => config,
        Err(message) => return pause(&message),
    };
    let config_file = config_dir().map_or(CONFIG_FILE.into(), |dir| dir.join(CONFIG_FILE));
    let notice = format!(
        "No projects found.\nSet `roots` in {}\nsee config.example.toml",
        config_file.display()
    );
    let chosen = picker::run(config::discover(&config), &notice).map_err(|err| err.to_string())?;
    let Some(project) = chosen else {
        return Ok(()); // Escape: leave everything as it is
    };
    if print_only {
        println!("{}", project.path.display());
        return Ok(());
    }
    match handoff::open_project(&herdr(), &pane, &project.path, config.direction) {
        Ok(()) => Ok(()),
        Err(message) => pause(&message),
    }
}

/// The popup closes as soon as the process exits, so hold an error on screen.
fn pause(message: &str) -> Result<(), String> {
    eprintln!("{message}\n\nPress Enter to close.");
    let _ = std::io::stdin().read_line(&mut String::new());
    Err(message.to_string())
}
