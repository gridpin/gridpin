//! The screen for one query, as text: cargo run --release --example snapshot -- <sheet.bin> "<address>" [width] [height]
use gridpin::query::Index;
use gridpin_tui::{draw, App, Key};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let idx = Index::open_address(std::path::Path::new(&args[1])).expect("sheet");
    let (w, h) = (
        args.get(3).map_or(76, |s| s.parse().unwrap()),
        args.get(4).map_or(12, |s| s.parse().unwrap()),
    );
    let mut app = App::default();
    for c in args[2].chars() {
        app.key(&idx, Key::Char(c));
    }
    app.settle(&idx);
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| draw(f, &app)).unwrap();
    let buf = t.backend().buffer().clone();
    for y in 0..h {
        println!(
            "{}",
            (0..w)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
                .trim_end()
        );
    }
}
