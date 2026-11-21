# gridpin-tui

A search box over a GridPin sheet in the terminal: candidates as you type, the details of the chosen one, and its point
copied with Enter. Offline: the sheet is a file on your machine.

```text
$ gridpin-tui france.bin      (typed: 1 rue de la Paix Paris)
┌ address ─────────────────────────────────────────────────────────────────┐
│1 rue de la Paix Paris                                                    │
└──────────────────────────────────────────────────────────────────────────┘
┌ candidates ────────────────────────────┐┌ details ───────────────────────┐
│1 Rue de la Paix, Paris 2e Arrondissemen││48.868546, 2.33031              │
│                                        ││precision house                 │
│                                        ││confidence 0.84                 │
│                                        ││postcode 75002                  │
│                                        ││                                │
│                                        ││                                │
└────────────────────────────────────────┘└────────────────────────────────┘
type an address · ↑↓ choose · Enter copies the point · Esc quits
```

```bash
cargo install --path gridpin-tui        # from code/ of the repository; Rust needed
gridpin-tui france.bin
```

- The list asks the sheet once a burst of typing is over, not on every key, so a large sheet (the United States, 1.45 GB)
  keeps up with typing.
- A weak answer — town level, low confidence — is marked `weak` in the list and in the details: check it before use.
- Enter puts `lat, lon` on the clipboard with the terminal's OSC 52 sequence (iTerm2, kitty, WezTerm, Windows Terminal,
  tmux with `set-clipboard on`); a terminal without it ignores the sequence.
- The window must be at least 40 × 10.

There is no map: the details name the point; give it to a map of your choice.

Not in the release archives: built from source (`THIRDPARTY.yml` lists the libraries it links; the two Windows-gnu
import crates of `winapi` ship no licence text the generator finds — they only matter for a Windows-gnu build).

Tests: `cargo test --release` in this folder (screens drawn on ratatui's TestBackend; the first candidate equals
`gridpin query`).
