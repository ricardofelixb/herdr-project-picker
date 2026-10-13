//! The popup's machine tabs and searchable project list: a pure state machine
//! plus the thin crossterm loop that drives it.

use crate::machine::Loaded;
use crate::snapshot::Row;
use crossterm::cursor::MoveTo;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::style::{Attribute, Print, SetAttribute};
use crossterm::terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, queue};
use std::io::{self, Write};
use std::ops::Range;
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Backspace,
    ClearQuery,
    Up,
    Down,
    Tab,
    BackTab,
    Enter,
    Esc,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Continue,
    /// Open `row`, which was loaded for the machine tab `tab`.
    Choose {
        tab: usize,
        row: Row,
    },
    Cancel,
}

/// A machine tab's projects. They are loaded when the tab is first shown and
/// kept for the life of the dialog.
enum Load {
    NotLoaded,
    Loading,
    Ready(Loaded),
    Failed(String),
}

struct Tab {
    label: String,
    load: Load,
}

enum Phase {
    Browsing,
    /// A project was chosen and Herdr is being asked to open it.
    Opening(String),
    /// Opening failed; shown until dismissed.
    Failed(String),
}

/// What the list area shows for the active tab.
pub enum Body<'a> {
    Loading(&'a str),
    Failed { label: &'a str, error: &'a str },
    Ready { note: Option<&'a str> },
}

pub struct State {
    tabs: Vec<Tab>,
    active: usize,
    query: String,
    matches: Vec<usize>,
    selected: usize,
    phase: Phase,
}

impl State {
    /// One tab per machine label; the first one is shown.
    pub fn new(labels: Vec<String>) -> State {
        assert!(!labels.is_empty(), "there is always a current machine");
        State {
            tabs: labels
                .into_iter()
                .map(|label| Tab {
                    label,
                    load: Load::NotLoaded,
                })
                .collect(),
            active: 0,
            query: String::new(),
            matches: Vec::new(),
            selected: 0,
            phase: Phase::Browsing,
        }
    }

    pub fn handle(&mut self, key: Key) -> Outcome {
        match self.phase {
            Phase::Browsing => {}
            // Escape stops waiting; whatever Herdr was already asked may still finish.
            Phase::Opening(_) => return Self::only_escape(key),
            Phase::Failed(_) => {
                return if matches!(key, Key::Enter | Key::Esc) {
                    Outcome::Cancel
                } else {
                    Outcome::Continue
                }
            }
        }
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
            Key::Tab => self.switch((self.active + 1) % self.tabs.len()),
            Key::BackTab => self.switch((self.active + self.tabs.len() - 1) % self.tabs.len()),
            Key::Enter => {
                if let Some(row) = self.visible().get(self.selected).map(|row| (*row).clone()) {
                    self.phase =
                        Phase::Opening(format!("{} on {}", row.name, self.tabs[self.active].label));
                    return Outcome::Choose {
                        tab: self.active,
                        row,
                    };
                }
            }
            Key::Esc => return Outcome::Cancel,
        }
        Outcome::Continue
    }

    /// A click does what its key would: a tab switches machine, a project row
    /// opens that project, the buttons open or close.
    pub fn click(&mut self, hit: Hit) -> Outcome {
        let browsing = matches!(self.phase, Phase::Browsing);
        match hit {
            Hit::Open => self.handle(Key::Enter),
            Hit::Close => self.handle(Key::Esc),
            Hit::Tab(tab) if browsing => {
                self.switch(tab);
                Outcome::Continue
            }
            Hit::Row(row) if browsing && row < self.matches.len() => {
                self.selected = row;
                self.handle(Key::Enter)
            }
            _ => Outcome::Continue,
        }
    }

    fn only_escape(key: Key) -> Outcome {
        if key == Key::Esc {
            Outcome::Cancel
        } else {
            Outcome::Continue
        }
    }

    fn switch(&mut self, tab: usize) {
        self.active = tab;
        // Coming back to a failed machine is the user asking to try it again.
        if matches!(self.tabs[tab].load, Load::Failed(_)) {
            self.tabs[tab].load = Load::NotLoaded;
        }
        self.refilter(); // the query stays, the selection starts over
    }

    /// The tab whose projects must be loaded now, if any: one that has not been
    /// shown yet, or a failed one the user just came back to. Never repeated
    /// on its own.
    pub fn take_load_request(&mut self) -> Option<usize> {
        let tab = &mut self.tabs[self.active];
        if matches!(tab.load, Load::NotLoaded) {
            tab.load = Load::Loading;
            return Some(self.active);
        }
        None
    }

    pub fn finish_load(&mut self, tab: usize, result: Result<Loaded, String>) {
        self.tabs[tab].load = match result {
            Ok(loaded) => Load::Ready(loaded),
            Err(error) => Load::Failed(error),
        };
        if tab == self.active {
            self.refilter();
        }
    }

    /// Opening failed: say so until the user dismisses it.
    pub fn open_failed(&mut self, error: String) {
        self.phase = Phase::Failed(error);
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn active(&self) -> usize {
        self.active
    }

    pub fn labels(&self) -> Vec<&str> {
        self.tabs.iter().map(|tab| tab.label.as_str()).collect()
    }

    pub fn body(&self) -> Body<'_> {
        let tab = &self.tabs[self.active];
        match &tab.load {
            Load::NotLoaded | Load::Loading => Body::Loading(&tab.label),
            Load::Failed(error) => Body::Failed {
                label: &tab.label,
                error,
            },
            Load::Ready(loaded) => Body::Ready {
                note: loaded.note.as_deref(),
            },
        }
    }

    /// Rows of the active tab matching the query, best first.
    pub fn visible(&self) -> Vec<&Row> {
        match &self.tabs[self.active].load {
            Load::Ready(loaded) => self.matches.iter().map(|&i| &loaded.rows[i]).collect(),
            _ => Vec::new(),
        }
    }

    fn refilter(&mut self) {
        let query = self.query.to_lowercase();
        let terms: Vec<&str> = query.split_whitespace().collect();
        let mut scored: Vec<(usize, u32)> = match &self.tabs[self.active].load {
            Load::Ready(loaded) => loaded
                .rows
                .iter()
                .enumerate()
                .filter_map(|(i, row)| score(&terms, row).map(|s| (i, s)))
                .collect(),
            _ => Vec::new(),
        };
        scored.sort_by_key(|&(_, points)| std::cmp::Reverse(points)); // stable: ties keep list order
        self.matches = scored.into_iter().map(|(i, _)| i).collect();
        self.selected = 0;
    }
}

/// Every term must match: a name substring scores highest (earlier is
/// better), then a name subsequence, then a substring of the full path.
fn score(terms: &[&str], row: &Row) -> Option<u32> {
    let name = row.name.to_lowercase();
    let path = row.path.to_lowercase();
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
        KeyCode::BackTab => Some(Key::BackTab),
        KeyCode::Tab if event.modifiers.contains(KeyModifiers::SHIFT) => Some(Key::BackTab),
        KeyCode::Tab => Some(Key::Tab),
        KeyCode::Enter => Some(Key::Enter),
        KeyCode::Esc => Some(Key::Esc),
        _ => None,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Mouse {
    /// A left click at a column and row.
    Click(usize, usize),
    ScrollUp,
    ScrollDown,
}

/// Maps a terminal mouse event: left clicks and the wheel. Moves, drags,
/// releases and other buttons do nothing.
pub fn map_mouse(event: MouseEvent) -> Option<Mouse> {
    match event.kind {
        MouseEventKind::Down(MouseButton::Left) => Some(Mouse::Click(
            usize::from(event.column),
            usize::from(event.row),
        )),
        MouseEventKind::ScrollUp => Some(Mouse::ScrollUp),
        MouseEventKind::ScrollDown => Some(Mouse::ScrollDown),
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

/// Which tabs of the given display `widths` are drawn in `width` columns:
/// the active one always, then its neighbors while they fit.
pub fn tab_window(widths: &[usize], active: usize, width: usize) -> Range<usize> {
    let mut used = widths[active].min(width);
    let mut start = active;
    while start > 0 && used + widths[start - 1] <= width {
        start -= 1;
        used += widths[start];
    }
    let mut end = active + 1;
    while end < widths.len() && used + widths[end] <= width {
        used += widths[end];
        end += 1;
    }
    start..end
}

/// How the dialog ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Finish {
    /// Escape, with nothing opened (or without waiting for it).
    Cancelled,
    /// The project opened; there was nothing to tell the user.
    Opened,
    /// Opening failed, or was reported and acknowledged.
    Failed(String),
}

/// Loads one tab's projects; called on a worker thread.
pub type LoadFn = Arc<dyn Fn(usize) -> Result<Loaded, String> + Send + Sync>;
/// Opens `row` of tab `tab`; the dialog closes once it succeeds.
pub type OpenFn = Arc<dyn Fn(usize, Row) -> Result<(), String> + Send + Sync>;

enum Msg {
    Key(Key),
    Mouse(Mouse),
    Redraw,
    Loaded(usize, Result<Loaded, String>),
    Opened(Result<(), String>),
    InputFailed(String),
}

/// Shows the dialog until it ends. The loop blocks on one channel that the
/// terminal reader and the load/open workers all send to, so it never polls
/// and a slow machine only delays its own tab.
pub fn run(labels: Vec<String>, load: LoadFn, open: OpenFn) -> io::Result<Finish> {
    let mut out = io::stdout();
    let _screen = Screen::enter()?;
    let (tx, rx) = mpsc::channel();
    spawn_input(tx.clone());
    let mut state = State::new(labels);
    loop {
        if let Some(tab) = state.take_load_request() {
            let (load, tx) = (load.clone(), tx.clone());
            std::thread::spawn(move || {
                let _ = tx.send(Msg::Loaded(tab, load(tab)));
            });
        }
        draw(&mut out, &state)?;
        let outcome = match rx.recv() {
            Ok(Msg::Key(key)) => state.handle(key),
            Ok(Msg::Mouse(Mouse::ScrollUp)) => state.handle(Key::Up),
            Ok(Msg::Mouse(Mouse::ScrollDown)) => state.handle(Key::Down),
            Ok(Msg::Mouse(Mouse::Click(column, row))) => {
                let (width, height) = terminal::size()?;
                match Layout::new(&state, width as usize, height as usize).hit(column, row) {
                    Some(hit) => state.click(hit),
                    None => Outcome::Continue,
                }
            }
            Ok(Msg::Loaded(tab, result)) => {
                state.finish_load(tab, result);
                Outcome::Continue
            }
            Ok(Msg::Opened(Ok(()))) => return Ok(Finish::Opened),
            Ok(Msg::Opened(Err(error))) => {
                state.open_failed(error);
                Outcome::Continue
            }
            Ok(Msg::Redraw) => Outcome::Continue,
            Ok(Msg::InputFailed(error)) => return Err(io::Error::other(error)),
            Err(_) => return Ok(Finish::Cancelled),
        };
        match outcome {
            Outcome::Continue => {}
            Outcome::Cancel => return Ok(finish(&state)),
            Outcome::Choose { tab, row } => {
                let (open, tx) = (open.clone(), tx.clone());
                std::thread::spawn(move || {
                    let _ = tx.send(Msg::Opened(open(tab, row)));
                });
            }
        }
    }
}

fn finish(state: &State) -> Finish {
    match &state.phase {
        Phase::Failed(error) => Finish::Failed(error.clone()),
        _ => Finish::Cancelled,
    }
}

/// Reads the terminal on its own thread, for the life of the process.
fn spawn_input(tx: Sender<Msg>) {
    std::thread::spawn(move || loop {
        let msg = match event::read() {
            Ok(Event::Key(event)) => map_key(event).map(Msg::Key),
            Ok(Event::Mouse(event)) => map_mouse(event).map(Msg::Mouse),
            Ok(Event::Resize(..)) => Some(Msg::Redraw),
            Ok(_) => None,
            Err(error) => {
                let _ = tx.send(Msg::InputFailed(error.to_string()));
                return;
            }
        };
        if let Some(msg) = msg {
            if tx.send(msg).is_err() {
                return;
            }
        }
    });
}

/// Raw mode on the alternate screen with mouse reporting, restored on drop.
struct Screen;

impl Screen {
    fn enter() -> io::Result<Screen> {
        terminal::enable_raw_mode()?;
        let screen = Screen;
        execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
        Ok(screen)
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

fn draw(out: &mut impl Write, state: &State) -> io::Result<()> {
    let (width, height) = terminal::size()?;
    render(out, state, width as usize, height as usize)
}

// The dialog's rows, laid out like Herdr's own dialogs: machine tabs, a rule,
// the search, a blank row, the list, then key hints and buttons at the bottom.
const TABS_ROW: usize = 0;
const RULE_ROW: usize = 1;
const SEARCH_ROW: usize = 2;
const LIST_TOP: usize = 4;
/// Text starts one column in; the rule and the selection bar span the width.
const MARGIN: usize = 1;
const OPEN: &str = " ↵ open ";
const CLOSE: &str = " esc close ";
const BUTTON_GAP: usize = 3;

/// What a click landed on.
#[derive(Debug, PartialEq, Eq)]
pub enum Hit {
    Tab(usize),
    /// A row of the visible list, counted from its first match.
    Row(usize),
    Open,
    Close,
}

/// Where each part of the dialog goes at one terminal size. Drawing and clicks
/// both read it, so a click always means what is on screen.
struct Layout {
    /// The tabs drawn: index, first column, text.
    tabs: Vec<(usize, usize, String)>,
    /// The rows between the search and the footer: messages, projects, a note.
    body: Range<usize>,
    /// How many body rows hold projects, and the first project shown.
    rows: usize,
    start: usize,
    /// The hints and buttons rows, when the dialog is tall enough for them.
    footer: Option<(usize, usize)>,
    open: Option<Range<usize>>,
    close: Range<usize>,
}

impl Layout {
    fn new(state: &State, width: usize, height: usize) -> Layout {
        let footer = (height >= LIST_TOP + 4).then(|| (height - 2, height - 1));
        let end = footer.map_or(height, |(hints, _)| hints - 1);
        let body = LIST_TOP.min(end)..end;
        let browsing = matches!(state.phase, Phase::Browsing);
        let len = if browsing { state.visible().len() } else { 0 };
        let note = len > 0 && matches!(state.body(), Body::Ready { note: Some(_) });
        let rows = body.len().saturating_sub(usize::from(note));
        let (open, close) = buttons(browsing, width);
        Layout {
            tabs: tabs(state, width),
            start: window_start(state.selected(), rows, len),
            body,
            rows,
            footer,
            open,
            close,
        }
    }

    fn hit(&self, column: usize, row: usize) -> Option<Hit> {
        if row == TABS_ROW {
            return (self.tabs.iter())
                .find(|(_, x, text)| (*x..x + text.chars().count()).contains(&column))
                .map(|(tab, ..)| Hit::Tab(*tab));
        }
        if (self.body.start..self.body.start + self.rows).contains(&row) {
            return Some(Hit::Row(self.start + row - self.body.start));
        }
        if self.footer.is_some_and(|(_, buttons)| buttons == row) {
            if self
                .open
                .as_ref()
                .is_some_and(|open| open.contains(&column))
            {
                return Some(Hit::Open);
            }
            if self.close.contains(&column) {
                return Some(Hit::Close);
            }
        }
        None
    }
}

/// The machine tabs that fit, the active one always, each followed by a gap.
fn tabs(state: &State, width: usize) -> Vec<(usize, usize, String)> {
    let labels: Vec<String> = (state.labels().iter())
        .map(|label| format!(" {} ", sanitize(label)))
        .collect();
    let widths: Vec<usize> = labels
        .iter()
        .map(|label| label.chars().count() + 1)
        .collect();
    let mut x = MARGIN;
    let mut drawn = Vec::new();
    for index in tab_window(&widths, state.active(), width.saturating_sub(MARGIN)) {
        let text = fit(&labels[index], width.saturating_sub(x));
        if text.is_empty() {
            break;
        }
        let next = x + text.chars().count() + 1;
        drawn.push((index, x, text));
        x = next;
    }
    drawn
}

/// The columns of the open and close buttons, centered. Only browsing can open.
fn buttons(browsing: bool, width: usize) -> (Option<Range<usize>>, Range<usize>) {
    let (open, close) = (OPEN.chars().count(), CLOSE.chars().count());
    if browsing {
        let x = width.saturating_sub(open + BUTTON_GAP + close) / 2;
        let close_x = x + open + BUTTON_GAP;
        (Some(x..x + open), close_x..close_x + close)
    } else {
        let x = width.saturating_sub(close) / 2;
        (None, x..x + close)
    }
}

/// The popup's frame and title are Herdr's own, and every style here is a plain
/// terminal attribute, so it follows the same palette as the panes.
fn render(out: &mut impl Write, state: &State, width: usize, height: usize) -> io::Result<()> {
    let layout = Layout::new(state, width, height);
    queue!(out, Clear(ClearType::All))?;
    for (index, x, text) in &layout.tabs {
        let style: &[Attribute] = if *index == state.active() {
            &[Attribute::Reverse, Attribute::Bold]
        } else {
            &[Attribute::Dim]
        };
        at(out, *x, TABS_ROW, text, style)?;
    }
    at(out, 0, RULE_ROW, &"─".repeat(width), &[Attribute::Dim])?;
    let search = fit(
        &format!("› {}", state.query()),
        width.saturating_sub(MARGIN),
    );
    at(out, MARGIN, SEARCH_ROW, &search, &[])?;
    if state.query().is_empty() {
        let hint = fit("search", width.saturating_sub(MARGIN + 2));
        at(out, MARGIN + 2, SEARCH_ROW, &hint, &[Attribute::Dim])?;
    }

    // Only the browsing screen has a list; the other phases own the whole body.
    let browsing = matches!(state.phase, Phase::Browsing);
    let visible = if browsing {
        state.visible()
    } else {
        Vec::new()
    };
    let (lines, note): (Vec<String>, Option<&str>) = match &state.phase {
        Phase::Opening(what) => (vec![format!("Opening {what}…")], None),
        Phase::Failed(error) => (
            vec![format!("Could not open: {error}"), "Enter to close".into()],
            None,
        ),
        Phase::Browsing => match state.body() {
            Body::Loading(label) => (vec![format!("Loading {label}…")], None),
            Body::Failed { label, error } => (vec![format!("{label} unavailable: {error}")], None),
            Body::Ready { note } if visible.is_empty() => {
                let text = if state.query().is_empty() {
                    note.unwrap_or("No projects found.")
                } else {
                    "No matching project"
                };
                (text.lines().map(str::to_string).collect(), None)
            }
            Body::Ready { note } => (Vec::new(), note),
        },
    };
    for (row, line) in lines.iter().take(layout.body.len()).enumerate() {
        put(out, layout.body.start + row, line, width)?;
    }
    if let (Some(note), Some(last)) = (note, layout.body.clone().last()) {
        put(out, last, note.lines().next().unwrap_or_default(), width)?;
    }

    for (row, project) in visible
        .iter()
        .skip(layout.start)
        .take(layout.rows)
        .enumerate()
    {
        let y = layout.body.start + row;
        if layout.start + row == state.selected() {
            let line = fit(&format!(" ▸ {}", project.name), width);
            let bar = format!("{line:<width$}");
            at(out, 0, y, &bar, &[Attribute::Reverse, Attribute::Bold])?;
        } else {
            at(out, 0, y, &fit(&format!("   {}", project.name), width), &[])?;
        }
    }

    if let Some((hints, buttons)) = layout.footer {
        if browsing {
            let tab = if state.labels().len() > 1 {
                "  tab machine"
            } else {
                ""
            };
            put(
                out,
                hints,
                &format!("↑↓ select{tab}  type to search"),
                width,
            )?;
        }
        if let Some(open) = &layout.open {
            let text = fit(OPEN, width.saturating_sub(open.start));
            at(
                out,
                open.start,
                buttons,
                &text,
                &[Attribute::Reverse, Attribute::Bold],
            )?;
        }
        let text = fit(CLOSE, width.saturating_sub(layout.close.start));
        at(out, layout.close.start, buttons, &text, &[Attribute::Bold])?;
    }

    let cursor = MARGIN + 2 + state.query().chars().count();
    queue!(
        out,
        MoveTo(
            cursor.min(width.saturating_sub(1)) as u16,
            SEARCH_ROW as u16
        )
    )?;
    out.flush()
}

/// `text`, already made safe with `fit`, at a column and row in `style`.
fn at(out: &mut impl Write, x: usize, y: usize, text: &str, style: &[Attribute]) -> io::Result<()> {
    queue!(out, MoveTo(x as u16, y as u16))?;
    for &attribute in style {
        queue!(out, SetAttribute(attribute))?;
    }
    queue!(out, Print(text), SetAttribute(Attribute::Reset))
}

/// One dim, indented line of messages.
fn put(out: &mut impl Write, row: usize, text: &str, width: usize) -> io::Result<()> {
    let text = fit(text, width.saturating_sub(2 * MARGIN));
    at(out, MARGIN, row, &text, &[Attribute::Dim])
}

/// Shows control characters (a directory name or a remote label can hold ESC)
/// escaped instead of sending them to the terminal.
pub fn sanitize(text: &str) -> String {
    text.chars()
        .flat_map(|c| {
            if c.is_control() {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

/// Makes `text` safe to print and cuts it to `width` characters, marking the
/// cut with an ellipsis.
fn fit(text: &str, width: usize) -> String {
    let text = sanitize(text);
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

    fn row(name: &str, parent: &str) -> Row {
        Row::new(&format!("{parent}/{name}"), None)
    }

    fn rows() -> Vec<Row> {
        vec![
            row("alpha", "/work"),
            row("beta-tools", "/work"),
            row("gamma", "/clients/acme"),
            row("tools", "/work"),
        ]
    }

    fn loaded(rows: Vec<Row>) -> Result<Loaded, String> {
        Ok(Loaded { rows, note: None })
    }

    /// A single-machine dialog whose projects have loaded.
    fn state() -> State {
        let mut state = State::new(vec!["Current".into()]);
        assert_eq!(state.take_load_request(), Some(0));
        state.finish_load(0, loaded(rows()));
        state
    }

    /// Current, Mini, Orgo with only the first one loaded.
    fn machines() -> State {
        let mut state = State::new(["Current", "Mini", "Orgo"].map(String::from).to_vec());
        state.take_load_request();
        state.finish_load(0, loaded(rows()));
        state
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
    fn enter_chooses_the_highlighted_project_of_the_active_machine() {
        let mut s = state();
        s.handle(Key::Down);
        assert_eq!(
            s.handle(Key::Enter),
            Outcome::Choose {
                tab: 0,
                row: row("beta-tools", "/work")
            }
        );
        // Opening: further keys are ignored, so it cannot be opened twice.
        assert_eq!(s.handle(Key::Enter), Outcome::Continue);
        assert_eq!(s.handle(Key::Char('x')), Outcome::Continue);
    }

    #[test]
    fn enter_without_a_match_does_nothing() {
        let mut s = state();
        type_text(&mut s, "zzz");
        assert_eq!(s.handle(Key::Enter), Outcome::Continue);
        let mut empty = State::new(vec!["Current".into()]);
        empty.take_load_request();
        empty.finish_load(0, loaded(vec![]));
        assert_eq!(empty.handle(Key::Enter), Outcome::Continue);
    }

    #[test]
    fn tab_and_backtab_cycle_machines_and_keep_the_query() {
        let mut s = machines();
        type_text(&mut s, "to");
        s.handle(Key::Down);
        s.handle(Key::Tab);
        assert_eq!((s.active(), s.query(), s.selected()), (1, "to", 0));
        s.handle(Key::Tab);
        s.handle(Key::Tab);
        assert_eq!(s.active(), 0, "wraps forward");
        s.handle(Key::BackTab);
        assert_eq!(s.active(), 2, "wraps backward");
        s.handle(Key::BackTab);
        assert_eq!(s.active(), 1);
        assert_eq!(s.labels(), ["Current", "Mini", "Orgo"]);
    }

    #[test]
    fn a_tab_loads_on_first_show_only_and_stays_cached() {
        let mut s = State::new(["Current", "Mini"].map(String::from).to_vec());
        assert_eq!(s.take_load_request(), Some(0));
        assert_eq!(s.take_load_request(), None, "already loading");
        s.finish_load(0, loaded(rows()));
        s.handle(Key::Tab);
        assert_eq!(s.take_load_request(), Some(1));
        assert_eq!(s.take_load_request(), None);
        s.finish_load(1, loaded(vec![row("remote", "/srv")]));
        s.handle(Key::BackTab);
        s.handle(Key::Tab);
        assert_eq!(s.take_load_request(), None, "no reload when coming back");
        assert_eq!(names(&s), ["remote"]);
    }

    #[test]
    fn a_failed_machine_is_retried_only_when_the_user_returns_to_it() {
        let mut s = machines();
        s.handle(Key::Tab);
        assert_eq!(s.take_load_request(), Some(1));
        s.finish_load(1, Err("timed out after 20s".into()));
        assert!(matches!(
            s.body(),
            Body::Failed {
                error: "timed out after 20s",
                ..
            }
        ));
        assert!(s.visible().is_empty(), "no other machine's list is shown");
        assert_eq!(s.take_load_request(), None, "no automatic retry");
        s.handle(Key::Tab);
        s.handle(Key::BackTab);
        assert_eq!(s.take_load_request(), Some(1));
        assert!(matches!(s.body(), Body::Loading("Mini")));
    }

    #[test]
    fn a_late_result_fills_its_own_tab_and_leaves_the_visible_one_alone() {
        let mut s = machines();
        s.handle(Key::Tab);
        assert_eq!(s.take_load_request(), Some(1));
        s.handle(Key::Tab); // user moves on while Mini is still loading
        assert_eq!(s.take_load_request(), Some(2));
        s.finish_load(1, loaded(vec![row("mini-app", "/m")]));
        assert!(matches!(s.body(), Body::Loading("Orgo")));
        assert!(s.visible().is_empty());
        s.handle(Key::BackTab);
        assert_eq!(names(&s), ["mini-app"]);
    }

    #[test]
    fn loading_and_unavailable_machines_cannot_be_chosen() {
        let mut s = machines();
        s.handle(Key::Tab);
        s.take_load_request();
        assert_eq!(s.handle(Key::Enter), Outcome::Continue);
        s.finish_load(1, Err("refused".into()));
        assert_eq!(s.handle(Key::Enter), Outcome::Continue);
    }

    #[test]
    fn a_chosen_remote_row_names_the_tab_it_was_loaded_for() {
        let mut s = machines();
        s.handle(Key::Tab);
        s.take_load_request();
        s.finish_load(1, loaded(vec![row("remote", "/srv")]));
        assert_eq!(
            s.handle(Key::Enter),
            Outcome::Choose {
                tab: 1,
                row: row("remote", "/srv")
            }
        );
    }

    #[test]
    fn escape_always_cancels() {
        let mut loading = machines();
        loading.handle(Key::Tab);
        loading.take_load_request();
        assert_eq!(loading.handle(Key::Esc), Outcome::Cancel);

        let mut failed = machines();
        failed.handle(Key::Tab);
        failed.take_load_request();
        failed.finish_load(1, Err("x".into()));
        assert_eq!(failed.handle(Key::Esc), Outcome::Cancel);

        let mut querying = state();
        type_text(&mut querying, "al");
        assert_eq!(querying.handle(Key::Esc), Outcome::Cancel);

        let mut opening = state();
        opening.handle(Key::Enter);
        assert_eq!(opening.handle(Key::Esc), Outcome::Cancel);
        assert_eq!(finish(&opening), Finish::Cancelled);

        let mut failed = state();
        failed.handle(Key::Enter);
        failed.open_failed("x".into());
        assert_eq!(failed.handle(Key::Char('x')), Outcome::Continue);
        assert_eq!(failed.handle(Key::Esc), Outcome::Cancel);
    }

    #[test]
    fn a_failed_open_is_held_until_dismissed_and_then_reported() {
        let mut s = state();
        s.handle(Key::Enter);
        s.open_failed("no such folder".into());
        assert_eq!(s.handle(Key::Enter), Outcome::Cancel);
        assert!(matches!(finish(&s), Finish::Failed(e) if e.contains("no such folder")));
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

    #[test]
    fn tab_keys_map_in_both_terminal_encodings_of_shift_tab() {
        use KeyEventKind::{Press, Release};
        let none = KeyModifiers::NONE;
        assert_eq!(map_key(key(KeyCode::Tab, none, Press)), Some(Key::Tab));
        assert_eq!(
            map_key(key(KeyCode::BackTab, KeyModifiers::SHIFT, Press)),
            Some(Key::BackTab)
        );
        assert_eq!(
            map_key(key(KeyCode::BackTab, none, Press)),
            Some(Key::BackTab)
        );
        assert_eq!(
            map_key(key(KeyCode::Tab, KeyModifiers::SHIFT, Press)),
            Some(Key::BackTab)
        );
        assert_eq!(map_key(key(KeyCode::Tab, none, Release)), None);
    }

    // Unix directory names may hold anything but `/` and NUL, including a
    // literal `ESC[2J` that would clear the popup if printed raw. Remote
    // labels, paths and error text are no more trusted.
    #[test]
    fn displayed_text_never_carries_terminal_controls() {
        let hostile = "evil\u{1b}[2J\u{7}\n\u{9b}name";
        let shown = fit(hostile, 80);
        assert!(!shown.chars().any(char::is_control), "{shown:?}");
        assert_eq!(shown, "evil\\u{1b}[2J\\u{7}\\n\\u{9b}name");
        assert_eq!(fit(hostile, 6).chars().count(), 6);
        assert_eq!(fit("héllo wörld", 80), "héllo wörld");
        assert_eq!(sanitize("Or\u{1b}go"), "Or\\u{1b}go");
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

    #[test]
    fn the_active_tab_is_always_drawn() {
        let widths = [9, 6, 7, 8];
        assert_eq!(tab_window(&widths, 0, 80), 0..4);
        assert_eq!(tab_window(&widths, 3, 80), 0..4);
        assert_eq!(tab_window(&widths, 3, 16), 2..4);
        assert_eq!(tab_window(&widths, 0, 16), 0..2);
        assert_eq!(tab_window(&widths, 2, 7), 2..3);
        assert_eq!(
            tab_window(&widths, 1, 3),
            1..2,
            "too narrow still shows the active tab"
        );
        assert_eq!(tab_window(&widths, 1, 0), 1..2);
    }

    fn screen(state: &State) -> String {
        let mut out = Vec::new();
        render(&mut out, state, 80, 12).unwrap();
        String::from_utf8(out).unwrap()
    }

    // The status, the result and the sidebar instructions are the whole point
    // of the screen after Enter; project rows must not be drawn over them.
    #[test]
    fn project_rows_are_only_drawn_while_browsing() {
        let mut state = state();
        assert!(screen(&state).contains("gamma"));

        state.handle(Key::Enter);
        let opening = screen(&state);
        assert!(opening.contains("Opening alpha on Current"), "{opening}");
        assert!(!opening.contains("gamma"), "{opening}");

        state.open_failed("no such folder".into());
        let failed = screen(&state);
        assert!(failed.contains("no such folder"), "{failed}");
        assert!(failed.contains("Enter to close"), "{failed}");
        assert!(!failed.contains("gamma"), "{failed}");
    }

    #[test]
    fn clicking_a_project_opens_it_and_a_tab_switches_machine() {
        let mut s = machines();
        assert_eq!(
            s.click(Hit::Row(2)),
            Outcome::Choose {
                tab: 0,
                row: row("gamma", "/clients/acme")
            }
        );
        assert_eq!(s.click(Hit::Row(1)), Outcome::Continue, "already opening");

        let mut s = machines();
        assert_eq!(s.click(Hit::Row(9)), Outcome::Continue, "below the list");
        assert_eq!(s.click(Hit::Tab(2)), Outcome::Continue);
        assert_eq!(s.active(), 2);
        assert_eq!(s.click(Hit::Close), Outcome::Cancel);
    }

    #[test]
    fn the_open_button_opens_the_highlighted_project() {
        let mut s = state();
        s.handle(Key::Down);
        assert_eq!(
            s.click(Hit::Open),
            Outcome::Choose {
                tab: 0,
                row: row("beta-tools", "/work")
            }
        );
    }

    // Clicks are read against the same layout that is drawn.
    #[test]
    fn clicks_land_on_what_is_drawn() {
        let mut s = machines();
        let layout = Layout::new(&s, 80, 12);
        // " Current " from column 1, a gap, then " Mini ".
        assert_eq!(layout.hit(1, TABS_ROW), Some(Hit::Tab(0)));
        assert_eq!(layout.hit(10, TABS_ROW), None, "the gap");
        assert_eq!(layout.hit(11, TABS_ROW), Some(Hit::Tab(1)));
        assert_eq!(layout.hit(30, SEARCH_ROW), None);
        assert_eq!(layout.hit(40, LIST_TOP + 1), Some(Hit::Row(1)));
        let open = layout.open.clone().unwrap();
        assert_eq!(layout.hit(open.start, 11), Some(Hit::Open));
        assert_eq!(layout.hit(layout.close.start, 11), Some(Hit::Close));
        assert_eq!(layout.hit(open.end, 11), None, "between the buttons");

        // Scrolled down, the first row on screen is the window's first match.
        for _ in 0..3 {
            s.handle(Key::Down);
        }
        let short = Layout::new(&s, 80, 9);
        assert_eq!(short.rows, 2);
        assert_eq!(short.hit(5, LIST_TOP), Some(Hit::Row(2)));
    }

    #[test]
    fn the_dialog_draws_herdrs_dialog_parts() {
        let mut s = machines();
        s.handle(Key::Down);
        let shown = screen(&s);
        for part in [
            "▸ beta-tools",
            "───",
            "↑↓ select  tab machine",
            "↵ open",
            "esc close",
        ] {
            assert!(shown.contains(part), "{part}: {shown}");
        }
        assert!(
            !screen(&state()).contains("tab machine"),
            "one machine, no tabs to switch"
        );
    }

    #[test]
    fn the_mouse_maps_left_clicks_and_the_wheel_only() {
        let event = |kind| MouseEvent {
            kind,
            column: 7,
            row: 3,
            modifiers: KeyModifiers::NONE,
        };
        let left = MouseButton::Left;
        assert_eq!(
            map_mouse(event(MouseEventKind::Down(left))),
            Some(Mouse::Click(7, 3))
        );
        assert_eq!(
            map_mouse(event(MouseEventKind::ScrollDown)),
            Some(Mouse::ScrollDown)
        );
        assert_eq!(
            map_mouse(event(MouseEventKind::ScrollUp)),
            Some(Mouse::ScrollUp)
        );
        assert_eq!(map_mouse(event(MouseEventKind::Up(left))), None);
        assert_eq!(map_mouse(event(MouseEventKind::Moved)), None);
        assert_eq!(
            map_mouse(event(MouseEventKind::Down(MouseButton::Right))),
            None
        );
    }

    // Names only: the list shows no folder paths.
    #[test]
    fn rows_show_names_without_paths() {
        let shown = screen(&state());
        assert!(shown.contains("gamma"), "{shown}");
        assert!(!shown.contains('/') && !shown.contains('\\'), "{shown}");
    }
}
