//! The popup's searchable project list: a pure state machine plus the thin
//! crossterm loop that drives it.

use crate::config::Project;
use crossterm::cursor::MoveTo;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::style::{Attribute, Print, SetAttribute};
use crossterm::terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, queue};
use std::io::{self, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Backspace,
    ClearQuery,
    Up,
    Down,
    Enter,
    Esc,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Continue,
    Choose(Project),
    Cancel,
}

pub struct State {
    projects: Vec<Project>,
    query: String,
    matches: Vec<usize>,
    selected: usize,
}

impl State {
    pub fn new(projects: Vec<Project>) -> State {
        let mut state = State {
            projects,
            query: String::new(),
            matches: Vec::new(),
            selected: 0,
        };
        state.refilter();
        state
    }

    pub fn handle(&mut self, key: Key) -> Outcome {
        match key {
            Key::Char(c) => {
                self.query.push(c);
                self.refilter();
            }
            Key::Backspace => {
                self.query.pop();
                self.refilter();
            }
            Key::ClearQuery => {
                self.query.clear();
                self.refilter();
            }
            Key::Up => self.selected = self.selected.saturating_sub(1),
            Key::Down => {
                if self.selected + 1 < self.matches.len() {
                    self.selected += 1;
                }
            }
            Key::Enter => {
                if let Some(&index) = self.matches.get(self.selected) {
                    return Outcome::Choose(self.projects[index].clone());
                }
            }
            Key::Esc => return Outcome::Cancel,
        }
        Outcome::Continue
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Projects matching the query, best first.
    pub fn visible(&self) -> Vec<&Project> {
        self.matches.iter().map(|&i| &self.projects[i]).collect()
    }

    fn refilter(&mut self) {
        let query = self.query.to_lowercase();
        let terms: Vec<&str> = query.split_whitespace().collect();
        let mut scored: Vec<(usize, u32)> = self
            .projects
            .iter()
            .enumerate()
            .filter_map(|(i, project)| score(&terms, project).map(|s| (i, s)))
            .collect();
        scored.sort_by_key(|&(_, points)| std::cmp::Reverse(points)); // stable: ties keep list order
        self.matches = scored.into_iter().map(|(i, _)| i).collect();
        self.selected = 0;
    }
}

/// Every term must match: a name substring scores highest (earlier is
/// better), then a name subsequence, then a substring of the full path.
fn score(terms: &[&str], project: &Project) -> Option<u32> {
    let name = project.name.to_lowercase();
    let path = project.path.to_string_lossy().to_lowercase();
    terms.iter().try_fold(0, |total, term| {
        let points = if let Some(at) = name.find(term) {
            1000 - at.min(500) as u32
        } else if is_subsequence(term, &name) {
            100
        } else if path.contains(term) {
            10
        } else {
            return None;
        };
        Some(total + points)
    })
}

fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut rest = haystack.chars();
    needle.chars().all(|c| rest.any(|h| h == c))
}

/// Maps a terminal key event to a picker key. Only key presses count:
/// Windows also reports releases and repeats.
pub fn map_key(event: KeyEvent) -> Option<Key> {
    if event.kind != KeyEventKind::Press {
        return None;
    }
    let ctrl = event.modifiers.contains(KeyModifiers::CONTROL);
    match event.code {
        KeyCode::Char(c) if ctrl => match c {
            'c' | 'g' => Some(Key::Esc),
            'n' => Some(Key::Down),
            'p' => Some(Key::Up),
            'u' => Some(Key::ClearQuery),
            _ => None,
        },
        KeyCode::Char(c) if !event.modifiers.contains(KeyModifiers::ALT) => Some(Key::Char(c)),
        KeyCode::Backspace => Some(Key::Backspace),
        KeyCode::Up => Some(Key::Up),
        KeyCode::Down => Some(Key::Down),
        KeyCode::Enter => Some(Key::Enter),
        KeyCode::Esc => Some(Key::Esc),
        _ => None,
    }
}

/// First row of a `rows`-tall window over `len` items that keeps `selected` in view.
pub fn window_start(selected: usize, rows: usize, len: usize) -> usize {
    if rows == 0 || len <= rows {
        return 0;
    }
    (selected + 1).saturating_sub(rows).min(len - rows)
}

/// Shows the picker until a project is chosen (`Some`) or it is cancelled
/// (`None`). `notice` explains an empty list. Input is read with blocking
/// reads, so nothing polls.
pub fn run(projects: Vec<Project>, notice: &str) -> io::Result<Option<Project>> {
    let mut out = io::stdout();
    let _screen = Screen::enter()?;
    let mut state = State::new(projects);
    loop {
        draw(&mut out, &state, notice)?;
        if let Event::Key(event) = event::read()? {
            let Some(key) = map_key(event) else { continue };
            match state.handle(key) {
                Outcome::Continue => {}
                Outcome::Choose(project) => return Ok(Some(project)),
                Outcome::Cancel => return Ok(None),
            }
        }
    }
}

/// Raw mode on the alternate screen, restored on drop.
struct Screen;

impl Screen {
    fn enter() -> io::Result<Screen> {
        terminal::enable_raw_mode()?;
        let screen = Screen;
        execute!(io::stdout(), EnterAlternateScreen)?;
        Ok(screen)
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

fn draw(out: &mut impl Write, state: &State, notice: &str) -> io::Result<()> {
    let (width, height) = terminal::size()?;
    let (width, height) = (width as usize, height as usize);
    let visible = state.visible();
    let list_rows = height.saturating_sub(3);
    let count = visible.len().to_string();

    queue!(
        out,
        Clear(ClearType::All),
        MoveTo(0, 0),
        SetAttribute(Attribute::Bold),
        Print(fit(" Open project", width.saturating_sub(count.len() + 2))),
        SetAttribute(Attribute::Reset),
        MoveTo(width.saturating_sub(count.len() + 1) as u16, 0),
        SetAttribute(Attribute::Dim),
        Print(&count),
        SetAttribute(Attribute::Reset),
        MoveTo(0, 1),
        Print(fit(&format!(" > {}", state.query()), width))
    )?;

    if visible.is_empty() {
        let text = if state.query().is_empty() {
            notice
        } else {
            "No matching project"
        };
        for (row, line) in text.lines().take(list_rows).enumerate() {
            queue!(
                out,
                MoveTo(1, 2 + row as u16),
                SetAttribute(Attribute::Dim),
                Print(fit(line, width.saturating_sub(2))),
                SetAttribute(Attribute::Reset)
            )?;
        }
    }
    let start = window_start(state.selected(), list_rows, visible.len());
    for (row, project) in visible.iter().skip(start).take(list_rows).enumerate() {
        let selected = start + row == state.selected();
        let line = format!(
            " {} {}  {}",
            if selected { '>' } else { ' ' },
            project.name,
            project.path.display()
        );
        queue!(out, MoveTo(0, 2 + row as u16))?;
        if selected {
            queue!(out, SetAttribute(Attribute::Reverse))?;
        }
        queue!(
            out,
            Print(format!("{:<width$}", fit(&line, width))),
            SetAttribute(Attribute::Reset)
        )?;
    }

    queue!(
        out,
        MoveTo(0, height.saturating_sub(1) as u16),
        SetAttribute(Attribute::Dim),
        Print(fit(" up/down move  enter open  esc cancel", width)),
        SetAttribute(Attribute::Reset),
        MoveTo(
            (4 + state.query().chars().count()).min(width.saturating_sub(1)) as u16,
            1
        )
    )?;
    out.flush()
}

/// Makes `text` safe to print and cuts it to `width` characters, marking the
/// cut with an ellipsis. Control characters (a directory name can hold ESC)
/// are shown escaped, never sent to the terminal; handoff keeps the real path.
fn fit(text: &str, width: usize) -> String {
    let text: String = text
        .chars()
        .flat_map(|c| {
            let shown = if c.is_control() {
                c.escape_default().to_string()
            } else {
                c.to_string()
            };
            shown.chars().collect::<Vec<_>>()
        })
        .collect();
    if text.chars().count() <= width {
        return text;
    }
    let kept: String = text.chars().take(width.saturating_sub(1)).collect();
    if width == 0 {
        kept
    } else {
        format!("{kept}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
    use std::path::PathBuf;

    fn project(name: &str, parent: &str) -> Project {
        Project {
            name: name.into(),
            path: PathBuf::from(parent).join(name),
        }
    }

    fn state() -> State {
        State::new(vec![
            project("alpha", "/work"),
            project("beta-tools", "/work"),
            project("gamma", "/clients/acme"),
            project("tools", "/work"),
        ])
    }

    fn type_text(state: &mut State, text: &str) {
        for c in text.chars() {
            state.handle(Key::Char(c));
        }
    }

    fn names(state: &State) -> Vec<&str> {
        state.visible().iter().map(|p| p.name.as_str()).collect()
    }

    #[test]
    fn empty_query_lists_everything_in_order() {
        assert_eq!(names(&state()), ["alpha", "beta-tools", "gamma", "tools"]);
    }

    #[test]
    fn typing_filters_and_ranks_name_matches_first() {
        let mut s = state();
        type_text(&mut s, "tools");
        assert_eq!(names(&s), ["tools", "beta-tools"]);
        s.handle(Key::ClearQuery);
        type_text(&mut s, "acme");
        assert_eq!(names(&s), ["gamma"]);
        s.handle(Key::ClearQuery);
        type_text(&mut s, "BT");
        assert_eq!(names(&s), ["beta-tools"]);
    }

    #[test]
    fn every_word_must_match() {
        let mut s = state();
        type_text(&mut s, "tools work");
        assert_eq!(names(&s), ["tools", "beta-tools"]);
        s.handle(Key::Char('x'));
        assert!(s.visible().is_empty());
    }

    #[test]
    fn backspace_widens_the_search_and_resets_selection() {
        let mut s = state();
        type_text(&mut s, "tools");
        s.handle(Key::Down);
        s.handle(Key::Backspace);
        assert_eq!(s.query(), "tool");
        assert_eq!(s.selected(), 0);
    }

    #[test]
    fn navigation_stays_inside_the_list() {
        let mut s = state();
        s.handle(Key::Up);
        assert_eq!(s.selected(), 0);
        for _ in 0..10 {
            s.handle(Key::Down);
        }
        assert_eq!(s.selected(), 3);
    }

    #[test]
    fn enter_chooses_the_highlighted_project() {
        let mut s = state();
        s.handle(Key::Down);
        assert_eq!(
            s.handle(Key::Enter),
            Outcome::Choose(project("beta-tools", "/work"))
        );
    }

    #[test]
    fn enter_without_a_match_does_nothing() {
        let mut s = state();
        type_text(&mut s, "zzz");
        assert_eq!(s.handle(Key::Enter), Outcome::Continue);
        assert_eq!(State::new(vec![]).handle(Key::Enter), Outcome::Continue);
    }

    #[test]
    fn escape_cancels_even_with_a_query() {
        let mut s = state();
        type_text(&mut s, "al");
        assert_eq!(s.handle(Key::Esc), Outcome::Cancel);
    }

    fn key(code: KeyCode, modifiers: KeyModifiers, kind: KeyEventKind) -> KeyEvent {
        let mut event = KeyEvent::new(code, modifiers);
        event.kind = kind;
        event
    }

    #[test]
    fn keys_map_presses_and_ignore_releases() {
        use KeyEventKind::{Press, Release};
        let none = KeyModifiers::NONE;
        let ctrl = KeyModifiers::CONTROL;
        assert_eq!(
            map_key(key(KeyCode::Char('a'), none, Press)),
            Some(Key::Char('a'))
        );
        assert_eq!(
            map_key(key(KeyCode::Char('A'), KeyModifiers::SHIFT, Press)),
            Some(Key::Char('A'))
        );
        assert_eq!(map_key(key(KeyCode::Char('a'), none, Release)), None);
        assert_eq!(map_key(key(KeyCode::Enter, none, Press)), Some(Key::Enter));
        assert_eq!(map_key(key(KeyCode::Esc, none, Press)), Some(Key::Esc));
        assert_eq!(
            map_key(key(KeyCode::Char('c'), ctrl, Press)),
            Some(Key::Esc)
        );
        assert_eq!(
            map_key(key(KeyCode::Char('n'), ctrl, Press)),
            Some(Key::Down)
        );
        assert_eq!(map_key(key(KeyCode::Char('p'), ctrl, Press)), Some(Key::Up));
        assert_eq!(
            map_key(key(KeyCode::Char('u'), ctrl, Press)),
            Some(Key::ClearQuery)
        );
        assert_eq!(map_key(key(KeyCode::Char('x'), ctrl, Press)), None);
        assert_eq!(map_key(key(KeyCode::F(1), none, Press)), None);
    }

    // Unix directory names may hold anything but `/` and NUL, including a
    // literal `ESC[2J` that would clear the popup if printed raw.
    #[test]
    fn displayed_text_never_carries_terminal_controls() {
        let hostile = "evil\u{1b}[2J\u{7}\n\u{9b}name";
        let shown = fit(hostile, 80);
        assert!(!shown.chars().any(char::is_control), "{shown:?}");
        assert_eq!(shown, "evil\\u{1b}[2J\\u{7}\\n\\u{9b}name");
        assert_eq!(fit(hostile, 6).chars().count(), 6);
        assert_eq!(fit("héllo wörld", 80), "héllo wörld");
    }

    #[test]
    fn window_follows_the_selection() {
        assert_eq!(window_start(0, 5, 20), 0);
        assert_eq!(window_start(4, 5, 20), 0);
        assert_eq!(window_start(5, 5, 20), 1);
        assert_eq!(window_start(19, 5, 20), 15);
        assert_eq!(window_start(2, 5, 3), 0);
        assert_eq!(window_start(0, 0, 3), 0);
    }
}
