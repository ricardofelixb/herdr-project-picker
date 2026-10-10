//! The popup's searchable project list: a pure state machine plus the thin
//! crossterm loop that drives it.

use crate::config::Project;
use crossterm::event::KeyEvent;

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
    pub fn new(_projects: Vec<Project>) -> State {
        todo!()
    }

    pub fn handle(&mut self, _key: Key) -> Outcome {
        todo!()
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
}

/// Maps a terminal key event to a picker key. Only key presses count:
/// Windows also reports releases and repeats.
pub fn map_key(_event: KeyEvent) -> Option<Key> {
    todo!()
}

/// First row of a `rows`-tall window over `len` items that keeps `selected` in view.
pub fn window_start(_selected: usize, _rows: usize, _len: usize) -> usize {
    todo!()
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
        assert_eq!(s.handle(Key::Enter), Outcome::Choose(project("beta-tools", "/work")));
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
        assert_eq!(map_key(key(KeyCode::Char('a'), none, Press)), Some(Key::Char('a')));
        assert_eq!(map_key(key(KeyCode::Char('A'), KeyModifiers::SHIFT, Press)), Some(Key::Char('A')));
        assert_eq!(map_key(key(KeyCode::Char('a'), none, Release)), None);
        assert_eq!(map_key(key(KeyCode::Enter, none, Press)), Some(Key::Enter));
        assert_eq!(map_key(key(KeyCode::Esc, none, Press)), Some(Key::Esc));
        assert_eq!(map_key(key(KeyCode::Char('c'), ctrl, Press)), Some(Key::Esc));
        assert_eq!(map_key(key(KeyCode::Char('n'), ctrl, Press)), Some(Key::Down));
        assert_eq!(map_key(key(KeyCode::Char('p'), ctrl, Press)), Some(Key::Up));
        assert_eq!(map_key(key(KeyCode::Char('u'), ctrl, Press)), Some(Key::ClearQuery));
        assert_eq!(map_key(key(KeyCode::Char('x'), ctrl, Press)), None);
        assert_eq!(map_key(key(KeyCode::F(1), none, Press)), None);
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
