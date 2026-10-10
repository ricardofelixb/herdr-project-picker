//! Herdr project picker plugin.
//!
//! Subcommands, wired up in `herdr-plugin.toml`:
//! - `open`: action; opens the picker popup over the focused pane.
//! - `pick`: popup entrypoint; machine tabs and a searchable project list.
//!   `pick --print` prints the chosen path instead of opening it.
//! - `bind-key --config PATH [--key prefix+f]`: adds the key to a Herdr config.

mod bind;
mod config;
mod handoff;
mod machine;
mod picker;
mod snapshot;

use config::{Config, CONFIG_FILE};
use handoff::HerdrCli;

use picker::{Finish, LoadFn, OpenFn};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

const ENV_ROOTS: &str = "HERDR_PROJECT_PICKER_ROOTS";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let result = match args.next().as_deref() {
        Some("open") => open(),
        Some("pick") => pick(args.any(|arg| arg == "--print")),
        Some("bind-key") => bind_key(args.collect()),
        _ => Err(
            "usage: herdr-project-picker open | pick [--print] | bind-key --config PATH [--key KEY]"
                .into(),
        ),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("herdr-project-picker: {}", printable(&message));
            ExitCode::FAILURE
        }
    }
}

/// Error text can come from a remote machine: show no terminal controls in it.
fn printable(message: &str) -> String {
    message
        .lines()
        .map(picker::sanitize)
        .collect::<Vec<_>>()
        .join("\n")
}

fn env(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("{name} is not set (is this running inside Herdr?)"))
}

fn herdr() -> HerdrCli {
    HerdrCli {
        bin: std::env::var_os("HERDR_BIN_PATH").map_or_else(|| "herdr".into(), PathBuf::from),
        timeout: handoff::CALL_TIMEOUT,
    }
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
    handoff::open_popup(&herdr(), &env("HERDR_PLUGIN_ID")?)
}

fn bind_key(args: Vec<String>) -> Result<(), String> {
    let usage = "usage: herdr-project-picker bind-key --config PATH [--key KEY]";
    let (mut config, mut key) = (None, bind::DEFAULT_KEY.to_string());
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => config = args.next().map(PathBuf::from),
            "--key" => key = args.next().ok_or(usage)?,
            _ => return Err(usage.into()),
        }
    }
    let config = config.ok_or(usage)?;
    println!("{}", bind::bind(&config, &key, &bind::action_id())?);
    Ok(())
}

fn pick(print_only: bool) -> Result<(), String> {
    let config = match load_config() {
        Ok(config) => config,
        Err(message) => return pause(&message),
    };
    let config_file = config_dir().map_or(CONFIG_FILE.into(), |dir| dir.join(CONFIG_FILE));
    let herdr = herdr();
    let (machines, catalog_warning) = machine::available(&herdr);
    let labels = machines.iter().map(|m| m.label.clone()).collect();
    let chosen = Arc::new(Mutex::new(None));

    let load: LoadFn = {
        let (herdr, machines, config) = (herdr.clone(), machines.clone(), config.clone());
        Arc::new(move |tab| {
            let loaded = machine::load(&machines[tab], &herdr, &config, &config_file)?;
            // The catalog problem belongs to the current machine's tab.
            let warning = catalog_warning.as_deref().filter(|_| tab == 0);
            Ok(loaded.with_warning(warning))
        })
    };
    let open: OpenFn = {
        let (herdr, chosen) = (herdr.clone(), chosen.clone());
        Arc::new(move |tab, row| {
            if print_only {
                *chosen.lock().unwrap() = Some(row.path);
                return Ok(None);
            }
            handoff::open_project(&herdr, &machines[tab], &row)
        })
    };

    match picker::run(labels, load, open).map_err(|err| err.to_string())? {
        Finish::Failed(message) => Err(message),
        Finish::Cancelled | Finish::Opened => {
            if let Some(path) = chosen.lock().unwrap().take() {
                println!("{path}");
            }
            Ok(())
        }
    }
}

/// The popup closes as soon as the process exits, so hold an error on screen.
fn pause(message: &str) -> Result<(), String> {
    eprintln!("{}\n\nPress Enter to close.", printable(message));
    let _ = std::io::stdin().read_line(&mut String::new());
    Err(message.to_string())
}
