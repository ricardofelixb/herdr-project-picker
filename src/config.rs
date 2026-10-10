//! Plugin configuration and project discovery. Everything is read from the
//! plugin config directory and the machine's own folders; nothing is baked in.

use serde::Deserialize;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

pub const CONFIG_FILE: &str = "config.toml";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    #[default]
    Right,
    Down,
}

impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::Right => "right",
            Direction::Down => "down",
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Config {
    /// Folders whose direct, non-hidden subfolders are projects.
    pub roots: Vec<PathBuf>,
    /// Folders that are projects themselves.
    pub projects: Vec<PathBuf>,
    pub direction: Direction,
    /// Opt-in: open the picker for new split panes. Off by default because
    /// Herdr cannot yet prove a pane was a user split.
    pub auto_trigger: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    roots: Vec<String>,
    #[serde(default)]
    projects: Vec<String>,
    #[serde(default)]
    direction: Direction,
    #[serde(default)]
    auto_trigger: bool,
}

impl Config {
    pub fn parse(text: &str, home: Option<&Path>) -> Result<Config, String> {
        let raw: RawConfig = toml::from_str(text).map_err(|err| err.to_string())?;
        let expand = |paths: Vec<String>| paths.iter().map(|p| expand_home(p, home)).collect();
        Ok(Config {
            roots: expand(raw.roots),
            projects: expand(raw.projects),
            direction: raw.direction,
            auto_trigger: raw.auto_trigger,
        })
    }

    /// Reads `config.toml` from `config_dir`; a missing file means defaults.
    /// `roots_override` (a PATH-style list) replaces the configured roots.
    pub fn load(
        config_dir: &Path,
        roots_override: Option<&OsStr>,
        home: Option<&Path>,
    ) -> Result<Config, String> {
        let path = config_dir.join(CONFIG_FILE);
        let mut config = match std::fs::read_to_string(&path) {
            Ok(text) => {
                Config::parse(&text, home).map_err(|e| format!("{}: {e}", path.display()))?
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(err) => return Err(format!("{}: {err}", path.display())),
        };
        if let Some(list) = roots_override {
            config.roots = std::env::split_paths(list).collect();
        }
        Ok(config)
    }
}

/// Expands a leading `~` or `~/` to `home`; other paths are left as written.
fn expand_home(path: &str, home: Option<&Path>) -> PathBuf {
    let Some(home) = home else {
        return PathBuf::from(path);
    };
    match path.strip_prefix('~') {
        Some("") => home.to_path_buf(),
        Some(rest) if rest.starts_with('/') || rest.starts_with('\\') => {
            home.join(rest.trim_start_matches(['/', '\\']))
        }
        _ => PathBuf::from(path),
    }
}

/// Lists the projects under the configured roots plus explicit projects,
/// sorted by name. Unreadable or missing folders are skipped.
pub fn discover(config: &Config) -> Vec<Project> {
    let mut paths: Vec<PathBuf> = config
        .projects
        .iter()
        .filter(|p| p.is_dir())
        .cloned()
        .collect();
    for root in &config.roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        paths.extend(
            entries
                .flatten()
                .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
                .map(|entry| entry.path())
                .filter(|path| path.is_dir()),
        );
    }
    let mut projects: Vec<Project> = paths
        .into_iter()
        .filter_map(|path| {
            let name = path.file_name()?.to_string_lossy().into_owned();
            Some(Project { name, path })
        })
        .collect();
    projects
        .sort_by(|a, b| (a.name.to_lowercase(), &a.path).cmp(&(b.name.to_lowercase(), &b.path)));
    projects.dedup_by(|a, b| a.path == b.path);
    projects
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> TempDir {
            let dir =
                std::env::temp_dir().join(format!("herdr-picker-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn empty_config_is_safe_by_default() {
        let config = Config::parse("", None).unwrap();
        assert!(config.roots.is_empty() && config.projects.is_empty());
        assert_eq!(config.direction, Direction::Right);
        assert!(!config.auto_trigger);
    }

    #[test]
    fn home_prefix_is_expanded_only_when_home_is_known() {
        let home = Path::new("/home/me");
        let text = r#"roots = ["~/code", "~", "/abs", "~other/x"]"#;
        let config = Config::parse(text, Some(home)).unwrap();
        assert_eq!(
            config.roots,
            [
                home.join("code"),
                home.to_path_buf(),
                PathBuf::from("/abs"),
                PathBuf::from("~other/x")
            ]
        );
        let config = Config::parse(r#"roots = ["~/code"]"#, None).unwrap();
        assert_eq!(config.roots, [PathBuf::from("~/code")]);
    }

    #[test]
    fn typos_and_bad_values_are_reported() {
        assert!(Config::parse("autotrigger = true", None).is_err());
        assert!(Config::parse(r#"direction = "left""#, None).is_err());
        assert!(Config::parse(r#"roots = "not-a-list""#, None).is_err());
        let config = Config::parse("direction = \"down\"\nauto_trigger = true", None).unwrap();
        assert_eq!(
            (config.direction, config.auto_trigger),
            (Direction::Down, true)
        );
    }

    #[test]
    fn missing_file_gives_defaults_and_override_replaces_roots() {
        let dir = TempDir::new("load");
        assert_eq!(Config::load(&dir.0, None, None).unwrap(), Config::default());

        fs::write(dir.0.join(CONFIG_FILE), r#"roots = ["/from/file"]"#).unwrap();
        let joined = std::env::join_paths(["/a", "/b"]).unwrap();
        let config = Config::load(&dir.0, Some(&joined), None).unwrap();
        assert_eq!(config.roots, [PathBuf::from("/a"), PathBuf::from("/b")]);
        let config = Config::load(&dir.0, None, None).unwrap();
        assert_eq!(config.roots, [PathBuf::from("/from/file")]);

        fs::write(dir.0.join(CONFIG_FILE), "roots = [").unwrap();
        assert!(Config::load(&dir.0, None, None).is_err());
    }

    #[test]
    fn discovery_lists_visible_folders_sorted_without_duplicates() {
        let root = TempDir::new("discover");
        for name in ["beta", "Alpha", ".hidden"] {
            fs::create_dir(root.0.join(name)).unwrap();
        }
        fs::write(root.0.join("notes.txt"), "").unwrap();
        let config = Config {
            roots: vec![root.0.clone(), root.0.join("missing")],
            projects: vec![root.0.join("beta"), root.0.join("also-missing")],
            ..Config::default()
        };
        let names: Vec<_> = discover(&config).into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["Alpha", "beta"]);
    }

    #[test]
    fn explicit_projects_are_listed_even_outside_roots() {
        let root = TempDir::new("explicit");
        fs::create_dir(root.0.join("solo")).unwrap();
        let config = Config {
            projects: vec![root.0.join("solo")],
            ..Config::default()
        };
        let projects = discover(&config);
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].path, root.0.join("solo"));
    }
}
