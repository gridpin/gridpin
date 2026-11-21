//! gridpin-tui <sheet.bin>: a search box over a GridPin sheet in the terminal. Nothing leaves the machine.
use std::io::Write;
use std::path::PathBuf;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use gridpin::query::Index;
use gridpin_tui::{draw, osc52, Action, App, Key};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

/// Puts the terminal back however the program ends, a panic included.
struct Restore;

impl Drop for Restore {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = crossterm::execute!(std::io::stdout(), LeaveAlternateScreen);
    }
}

fn main() {
    let Some(sheet) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: gridpin-tui <sheet.bin>");
        std::process::exit(2);
    };
    let idx = match Index::open_address(&sheet) {
        Ok(idx) => idx,
        Err(e) => {
            eprintln!("gridpin-tui: {e:#}");
            std::process::exit(1);
        }
    };
    if let Err(e) = run(&idx) {
        eprintln!("gridpin-tui: {e}");
        std::process::exit(1);
    }
}

fn run(idx: &Index) -> std::io::Result<()> {
    enable_raw_mode()?;
    let _restore = Restore;
    crossterm::execute!(std::io::stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    let mut app = App::default();
    loop {
        if !event::poll(std::time::Duration::ZERO)? {
            app.settle(idx); // no key waiting: answer what is typed, then draw
            terminal.draw(|f| draw(f, &app))?;
        }
        let Event::Key(k) = event::read()? else {
            continue;
        };
        if k.kind != KeyEventKind::Press {
            continue;
        }
        let key = match k.code {
            KeyCode::Char(c) => Key::Char(c),
            KeyCode::Backspace => Key::Backspace,
            KeyCode::Up => Key::Up,
            KeyCode::Down => Key::Down,
            KeyCode::Enter => Key::Enter,
            KeyCode::Esc => Key::Esc,
            _ => continue,
        };
        match app.key(idx, key) {
            Action::Quit => return Ok(()),
            Action::Copy(point) => {
                print!("{}", osc52(&point));
                std::io::stdout().flush()?;
            }
            Action::None => {}
        }
    }
}
