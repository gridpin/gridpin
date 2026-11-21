//! The state and the drawing of `gridpin-tui`, apart from the terminal so they can be tested with ratatui's TestBackend.
use gridpin::query::{hit_is_weak, query_cascade, Hit, Index};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

/// Candidates shown for the typed text.
pub const CANDIDATES: usize = 8;
/// The smallest window the layout fits in.
pub const MIN: (u16, u16) = (40, 10);

#[derive(Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Backspace,
    Up,
    Down,
    Enter,
    Esc,
}

#[derive(Debug, PartialEq)]
pub enum Action {
    None,
    /// "lat, lon" to put on the clipboard
    Copy(String),
    Quit,
}

#[derive(Default)]
pub struct App {
    pub input: String,
    pub hits: Vec<Hit>,
    pub selected: usize,
    pub status: String,
    /// typed since the last question to the sheet: asked once a burst of keys is over (`settle`)
    pub dirty: bool,
}

impl App {
    /// Ask the sheet again for the current input (an empty input clears the list).
    pub fn refresh(&mut self, idx: &Index) {
        self.hits = if self.input.trim().is_empty() {
            Vec::new()
        } else {
            query_cascade(idx, None, self.input.trim(), CANDIDATES)
        };
        self.selected = 0;
        self.status = match self.hits.len() {
            0 if !self.input.trim().is_empty() => "no answer".into(),
            _ => String::new(),
        };
    }

    /// Ask the sheet if the input changed since the last answer. The terminal loop calls it when no key is waiting, so
    /// a burst of typing costs one question, not one per key.
    pub fn settle(&mut self, idx: &Index) {
        if self.dirty {
            self.dirty = false;
            self.refresh(idx);
        }
    }

    pub fn key(&mut self, idx: &Index, key: Key) -> Action {
        if !matches!(key, Key::Char(_) | Key::Backspace) {
            self.settle(idx); // a choice is made on the answers to what is typed now
        }
        match key {
            Key::Char(c) => {
                self.input.push(c);
                self.dirty = true;
            }
            Key::Backspace => {
                self.input.pop();
                self.dirty = true;
            }
            Key::Up => self.selected = self.selected.saturating_sub(1),
            Key::Down => {
                if self.selected + 1 < self.hits.len() {
                    self.selected += 1;
                }
            }
            Key::Enter => {
                if let Some(h) = self.hits.get(self.selected) {
                    let point = format!("{}, {}", h.lat, h.lon);
                    self.status = format!("copied {point}");
                    return Action::Copy(point);
                }
            }
            Key::Esc => return Action::Quit,
        }
        Action::None
    }
}

fn place(h: &Hit) -> String {
    let street = [h.housenumber.as_deref().unwrap_or(""), h.street.as_str()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    [street.as_str(), h.commune.as_str()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    if area.width < MIN.0 || area.height < MIN.1 {
        frame.render_widget(
            Paragraph::new(format!("window too small: {}x{} at least", MIN.0, MIN.1)),
            area,
        );
        return;
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(4),
            Constraint::Length(1),
        ])
        .split(area);
    frame.render_widget(
        Paragraph::new(app.input.as_str())
            .block(Block::default().borders(Borders::ALL).title(" address ")),
        rows[0],
    );
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(rows[1]);
    let items: Vec<ListItem> = app
        .hits
        .iter()
        .map(|h| {
            let weak = hit_is_weak(h);
            let mut spans = vec![Span::raw(place(h))];
            if weak {
                spans.push(Span::styled(" weak", Style::default().fg(Color::Yellow)));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let mut state = ListState::default();
    if !app.hits.is_empty() {
        state.select(Some(app.selected));
    }
    frame.render_stateful_widget(
        List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" candidates "))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
        cols[0],
        &mut state,
    );
    frame.render_widget(
        Paragraph::new(details(app.hits.get(app.selected)))
            .block(Block::default().borders(Borders::ALL).title(" details ")),
        cols[1],
    );
    frame.render_widget(
        Paragraph::new(if app.status.is_empty() {
            "type an address · ↑↓ choose · Enter copies the point · Esc quits".into()
        } else {
            app.status.clone()
        }),
        rows[2],
    );
}

fn details(h: Option<&Hit>) -> Vec<Line<'static>> {
    let Some(h) = h else {
        return vec![Line::raw("")];
    };
    let weak = hit_is_weak(h);
    vec![
        Line::raw(format!("{}, {}", h.lat, h.lon)),
        Line::raw(format!("precision {}", h.precision)),
        Line::raw(format!("confidence {:.2}", h.confidence)),
        Line::raw(format!("postcode {}", h.postcode)),
        if weak {
            Line::from(Span::styled(
                "weak: check it before use",
                Style::default().fg(Color::Yellow),
            ))
        } else {
            Line::raw("")
        },
    ]
}

/// The OSC 52 sequence that asks the terminal to put `text` on the clipboard (no clipboard library needed).
pub fn osc52(text: &str) -> String {
    const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = text.as_bytes();
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, b)| acc | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(B64[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    format!("\x1b]52;c;{out}\x07")
}
