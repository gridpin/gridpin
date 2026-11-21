//! Each state of the screen on a two-street fixture sheet, drawn on ratatui's TestBackend.
use std::path::PathBuf;
use std::process::Command;

use gridpin::query::Index;
use gridpin_tui::{draw, osc52, Action, App, Key};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

fn sheet() -> (PathBuf, Index) {
    let dir = std::env::temp_dir().join(format!(
        "gridpin-tui-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let csv = dir.join("in.csv");
    std::fs::write(&csv, "nom_voie_norm,code_insee,nom_commune_norm,code_postal,numero,rep,lon,lat,nom_voie,nom_commune\n\
        rue a,001,ville,10000,1,,7.42,43.73,Rue A,Ville\nrue b,001,ville,10000,2,,7.50,43.80,Rue B,Ville\n").unwrap();
    let man = dir.join("m.json");
    std::fs::write(
        &man,
        r#"{"country":"mc","layer":"addresses","license":"t","source_release":"test"}"#,
    )
    .unwrap();
    let bin = dir.join("s.bin");
    let gridpin =
        std::env::var("GRIDPIN_BIN").unwrap_or_else(|_| "../gridpin/target/release/gridpin".into());
    let out = Command::new(gridpin)
        .args([
            "build",
            csv.to_str().unwrap(),
            bin.to_str().unwrap(),
            "--meta",
            man.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let idx = Index::open_address(&bin).unwrap();
    (bin, idx)
}

fn screen(app: &App, w: u16, h: u16) -> String {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| draw(f, app)).unwrap();
    let buf = t.backend().buffer().clone();
    (0..h)
        .map(|y| {
            (0..w)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn typed(idx: &Index, text: &str) -> App {
    let mut app = App::default();
    for c in text.chars() {
        app.key(idx, Key::Char(c));
    }
    app.settle(idx);
    app
}

#[test]
fn candidates_follow_the_typing_and_the_first_is_what_query_gives() {
    let (bin, idx) = sheet();
    let app = typed(&idx, "1 rue a ville");
    let s = screen(&app, 80, 14);
    assert!(s.contains("1 Rue A, Ville"), "{s}");
    assert!(s.contains("precision house"), "{s}");
    let gridpin =
        std::env::var("GRIDPIN_BIN").unwrap_or_else(|_| "../gridpin/target/release/gridpin".into());
    let q = Command::new(gridpin)
        .args(["query", bin.to_str().unwrap(), "1 rue a ville", "-k", "1"])
        .output()
        .unwrap();
    let first: serde_json::Value =
        serde_json::from_slice(q.stdout.split(|b| *b == b'\n').next().unwrap()).unwrap();
    assert_eq!(app.hits[0].lat, first["lat"].as_f64().unwrap());
    assert_eq!(app.hits[0].lon, first["lon"].as_f64().unwrap());
}

#[test]
fn a_weak_answer_says_so_in_the_list_and_the_details() {
    let (_bin, idx) = sheet();
    let app = typed(&idx, "rue a");
    let s = screen(&app, 80, 14);
    assert!(s.contains(" weak"), "{s}");
    assert!(s.contains("weak: check it before use"), "{s}");
}

#[test]
fn enter_copies_the_selected_point_and_arrows_move() {
    let (_bin, idx) = sheet();
    let mut app = typed(&idx, "ville");
    let n = app.hits.len();
    assert!(n >= 1);
    app.key(&idx, Key::Up);
    assert_eq!(app.selected, 0, "no row above the first");
    for _ in 0..n + 3 {
        app.key(&idx, Key::Down);
    }
    assert_eq!(app.selected, n - 1, "no row below the last");
    let h = &app.hits[n - 1];
    let want = format!("{}, {}", h.lat, h.lon);
    assert_eq!(app.key(&idx, Key::Enter), Action::Copy(want.clone()));
    assert!(screen(&app, 80, 14).contains(&format!("copied {want}")));
    assert_eq!(app.key(&idx, Key::Esc), Action::Quit);
}

#[test]
fn empty_input_no_answer_and_a_small_window() {
    let (_bin, idx) = sheet();
    let mut app = typed(&idx, "zzzz qqqq");
    assert!(screen(&app, 80, 14).contains("no answer"));
    assert_eq!(app.key(&idx, Key::Enter), Action::None, "nothing to copy");
    for _ in 0.."zzzz qqqq".len() {
        app.key(&idx, Key::Backspace);
    }
    app.settle(&idx);
    assert!(app.hits.is_empty() && app.status.is_empty());
    assert!(screen(&app, 39, 10).contains("window too small"));
    assert!(screen(&app, 40, 10).contains("candidates"));
}

#[test]
fn a_burst_of_keys_asks_once_and_enter_sees_the_whole_text() {
    let (_bin, idx) = sheet();
    let mut app = App::default();
    for c in "1 rue b".chars() {
        app.key(&idx, Key::Char(c));
    }
    assert!(
        app.hits.is_empty() && app.dirty,
        "nothing asked while keys are coming"
    );
    for c in " ville".chars() {
        app.key(&idx, Key::Char(c));
    }
    // Enter without a settle in between: the point is that of "1 rue b ville", not of a part of it
    let Action::Copy(point) = app.key(&idx, Key::Enter) else {
        panic!("nothing copied")
    };
    assert_eq!(point, "43.8, 7.5");
}

#[test]
fn the_clipboard_sequence_is_osc52_with_base64() {
    assert_eq!(osc52("43.73, 7.42"), "\x1b]52;c;NDMuNzMsIDcuNDI=\x07");
    assert_eq!(osc52("ab"), "\x1b]52;c;YWI=\x07");
    assert_eq!(osc52("abc"), "\x1b]52;c;YWJj\x07");
}
