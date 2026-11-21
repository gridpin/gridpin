//! Query-string normalization — mirrors DuckDB strip_accents(lower(...)):
//! lowercase → NFD decomposition → strip combining marks →
//! punctuation to space → collapse whitespace.
//!
//! NFD covers all scripts automatically, with no manual lists: French é/ç,
//! Serbian š/ž/č/ć, Russian yo/short-i (which decompose to plain e/i).
//! Ligatures (œ, æ) do not decompose and are kept as is. EXCEPTION: đ (U+0111)
//! does not decompose under NFD, so it is folded to d explicitly (the index
//! build does the same) — otherwise input like "Karadordeva" typed without
//! a Serbian layout would not match "Karađorđeva".

use unicode_normalization::char::is_combining_mark;
use unicode_normalization::UnicodeNormalization;

/// A query with typographic apostrophes written as the ASCII sign the sheets were built from ("D´Avila" →
/// "D'Avila"), and a typographic dash between spaces, a separator ("4 – 10117 Berlin"), written as a hyphen: applied
/// to the query only, so the keys of sheets already built stay as they are (plan K2.1). A dash inside a word or a
/// number range ("93–99") is kept: on the 59 open sets of 17.11.2026 turning it into a hyphen lost two right answers
/// (a range without a town, where the choice among same-named streets changed). Double quotes are kept as written:
/// the keys keep them, and a sheet holds both forms (Poland: 976 street rows with „…”, 3 944 with "…"), so writing
/// „Solidarności” as "Solidarności" sent Warsaw's avenue to Opole's (21.11.2026, the Polish clean set, three rows).
pub fn ascii_signs(s: &str) -> std::borrow::Cow<'_, str> {
    const SIGNS: [(char, char); 9] = [
        ('\u{2019}', '\''),
        ('\u{2018}', '\''),
        ('\u{02BC}', '\''),
        ('\u{00B4}', '\''),
        ('`', '\''),
        ('\u{2010}', '-'),
        ('\u{2011}', '-'),
        ('\u{2013}', '-'),
        ('\u{2014}', '-'),
    ];
    if !s.chars().any(|c| SIGNS.iter().any(|(from, _)| *from == c)) {
        return std::borrow::Cow::Borrowed(s);
    }
    let ch: Vec<char> = s.chars().collect();
    let spaced = |i: usize| {
        i > 0 && i + 1 < ch.len() && ch[i - 1].is_whitespace() && ch[i + 1].is_whitespace()
    };
    let mut out = String::with_capacity(s.len());
    // after a range dash: the spaces before the second number are dropped
    let mut skip_spaces = false;
    for (i, &c) in ch.iter().enumerate() {
        if skip_spaces && c.is_whitespace() {
            continue;
        }
        skip_spaces = false;
        match SIGNS.iter().find(|(from, _)| *from == c) {
            Some((_, '-')) if !spaced(i) => out.push(c),
            Some((_, '-')) => {
                // "17 – 21": two numbers around a spaced dash are the range "17-21" the number reader knows; any
                // other spaced dash is a separator, " - "
                let first: String = out
                    .trim_end()
                    .chars()
                    .rev()
                    .take_while(char::is_ascii_digit)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                let second: String = ch[i + 1..]
                    .iter()
                    .skip_while(|d| d.is_whitespace())
                    .take_while(|d| d.is_ascii_digit())
                    .collect();
                // a range: the second number is bigger and at most one digit longer ("17 – 21"); "4 – 10117 Berlin"
                // is a house and a postcode, "1 – 329" a flat and a house
                let range = match (first.parse::<u64>(), second.parse::<u64>()) {
                    (Ok(a), Ok(b)) => b > a && second.len() <= first.len() + 1,
                    _ => false,
                };
                if range {
                    out.truncate(out.trim_end().len());
                    skip_spaces = true;
                }
                out.push('-');
            }
            Some((_, to)) => out.push(*to),
            None => out.push(c),
        }
    }
    std::borrow::Cow::Owned(out)
}

/// A line break of a web page left in a query ("Place de la Gare<br />67630 Lauterbourg") is a separator, not
/// part of the street: `<br>`, `<br/>`, `<br />` in any case become ", " (17.11.2026: 29 rows of the French clean
/// set, 25 of other sets; with the tag the place read "Rue de la Gare").
pub fn without_line_break_tags(s: &str) -> std::borrow::Cow<'_, str> {
    let lower = s.to_ascii_lowercase();
    if !lower.contains("<br") {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if lower[i..].starts_with("<br") {
            let rest = &lower[i + 3..];
            let tail = rest.trim_start_matches(' ');
            let tail = tail.strip_prefix('/').unwrap_or(tail);
            if let Some(after) = tail.strip_prefix('>') {
                out.push_str(", ");
                i = s.len() - after.len();
                continue;
            }
        }
        let c = s[i..].chars().next().unwrap_or(' ');
        out.push(c);
        i += c.len_utf8();
    }
    std::borrow::Cow::Owned(out)
}

/// The line with each HTML entity (`&nbsp;`, `&amp;`, `&#8211;`) read as a space.
fn without_html_entities(q: &str) -> String {
    let mut out = String::with_capacity(q.len());
    let mut rest = q;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let tail = &rest[at + 1..];
        let name_len = tail.find(';').filter(|&n| {
            (1..=8).contains(&n)
                && tail[..n]
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '#')
        });
        match name_len {
            Some(n) => {
                out.push(' ');
                rest = &tail[n + 1..];
            }
            None => {
                out.push('&');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The addresses a line lists for one place, each with the town its tail names: "1 quai Bourbon ; 3 rue des Deux-Ponts,
/// Paris 4e" -> ["1 quai Bourbon, Paris 4e", "3 rue des Deux-Ponts, Paris 4e"]. A monument or a corner building is written
/// with all its doors (17.11.2026, French Mérimée set: 150 of 1 000 rows). Empty when the line lists no second address.
pub fn listed_addresses(q: &str) -> Vec<String> {
    // an HTML entity ("van&nbsp;de&nbsp;Heirweg") ends with ";" and separates nothing
    let q = without_html_entities(q);
    if !q.contains(';') {
        return Vec::new();
    }
    let mut segs: Vec<String> = q.split(';').map(|s| s.trim().to_string()).collect();
    let last = segs.pop().unwrap_or_default();
    let (last_address, town) = match last.rsplit_once(',') {
        Some((a, t)) if !t.trim().is_empty() => (a.trim().to_string(), t.trim().to_string()),
        _ => (last, String::new()),
    };
    // without a town in the tail each door would be read anywhere ("400 Rusk Street; Houston TX 77002" -> Tyler)
    if town.is_empty() {
        return Vec::new();
    }
    segs.push(last_address);
    segs.into_iter()
        .filter(|s| s.chars().any(char::is_alphanumeric))
        .map(|s| {
            if town.is_empty() || s.contains(&town) {
                s
            } else {
                format!("{s}, {town}")
            }
        })
        .collect()
}

/// The line without the notes it carries about its place: a parenthetical ("Baltimore (Independent City)", "1511
/// (rear) Twenty-second St.") and the word "Borough" of a town segment ("Borough of West Cape May", "Stone Harbor
/// Borough"). None when there is nothing to drop. The sheet names the town alone, so the note made the town unread
/// (17.11.2026, US NRHP messy set: +13, no loss, where the census found the house and we did not).
pub fn without_place_notes(q: &str) -> Option<String> {
    // only a note on the kind of place: a parenthetical often carries the address or the place itself ("LASALLE, QC
    // (7400 ST-PATRICK)", "Ypekolsgea (Súdwest-Fryslân)"), and dropping those sent answers hundreds of km away
    const NOTES: &[&str] = &[
        "independent city",
        "city",
        "town",
        "village",
        "township",
        "borough",
        "rear",
    ];
    let mut out = String::with_capacity(q.len());
    let mut rest = q;
    while let Some(open) = rest.find('(') {
        let Some(close) = rest[open..].find(')').map(|c| open + c) else {
            break; // an unclosed parenthesis: not a note
        };
        let inner = rest[open + 1..close].trim().to_lowercase();
        out.push_str(&rest[..open]);
        if !NOTES.contains(&inner.as_str()) {
            out.push_str(&rest[open..=close]);
        }
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    let segs: Vec<String> = out
        .split(',')
        .map(|seg| {
            let t = seg.split_whitespace().collect::<Vec<_>>().join(" ");
            let low = t.to_lowercase();
            let t = if low.starts_with("borough of ") && t.len() > 11 {
                t[11..].to_string()
            } else if low.ends_with(" borough")
                && t.len() > 8
                && !low.chars().any(|c| c.is_ascii_digit())
            {
                t[..t.len() - 8].to_string()
            } else {
                t
            };
            t
        })
        .collect();
    let joined = segs.join(", ");
    let before = q
        .split(',')
        .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join(", ");
    (joined != before && joined.chars().any(char::is_alphanumeric)).then_some(joined)
}

/// The line with its English spelled ordinals written as numbers: "620 S. Eighth St." -> "620 S. 8th St.",
/// "461 Twenty-seventh St." -> "461 27th St.". The sheet keys the number, and the spelled word together with an
/// abbreviated direction or type ran out of street variants (17.11.2026, US NRHP messy set). None without one.
pub fn with_numbered_ordinals(q: &str) -> Option<String> {
    const UNITS: &[(&str, u32)] = &[
        ("first", 1),
        ("second", 2),
        ("third", 3),
        ("fourth", 4),
        ("fifth", 5),
        ("sixth", 6),
        ("seventh", 7),
        ("eighth", 8),
        ("ninth", 9),
        ("tenth", 10),
        ("eleventh", 11),
        ("twelfth", 12),
        ("thirteenth", 13),
        ("fourteenth", 14),
        ("fifteenth", 15),
        ("sixteenth", 16),
        ("seventeenth", 17),
        ("eighteenth", 18),
        ("nineteenth", 19),
        ("twentieth", 20),
        ("thirtieth", 30),
        ("fortieth", 40),
        ("fiftieth", 50),
    ];
    const TENS: &[(&str, u32)] = &[("twenty", 20), ("thirty", 30), ("forty", 40), ("fifty", 50)];
    let suffix = |n: u32| match (n % 100, n % 10) {
        (11..=13, _) => "th",
        (_, 1) => "st",
        (_, 2) => "nd",
        (_, 3) => "rd",
        _ => "th",
    };
    let unit = |w: &str| {
        UNITS
            .iter()
            .find(|(u, _)| u.eq_ignore_ascii_case(w))
            .map(|(_, n)| *n)
    };
    let mut changed = false;
    let words: Vec<String> = q
        .split(' ')
        .map(|word| {
            let core_end = word.trim_end_matches(|c: char| !c.is_alphabetic()).len();
            let (core, rest) = word.split_at(core_end);
            let n = match core.split_once('-') {
                Some((tens, one)) => TENS
                    .iter()
                    .find(|(t, _)| t.eq_ignore_ascii_case(tens))
                    .zip(unit(one).filter(|n| *n < 10))
                    .map(|((_, t), u)| t + u),
                None => unit(core),
            };
            match n {
                Some(n) => {
                    changed = true;
                    format!("{n}{}{rest}", suffix(n))
                }
                None => word.to_string(),
            }
        })
        .collect();
    changed.then(|| words.join(" "))
}

const QUERY_BOX_WORDS: &str = include_str!("query_box_words.tsv");

/// The query without its post office box and its CEDEX mark: "246 rue Saint Jean BP 70639 59500 Douai" ->
/// "246 rue Saint Jean 59500 Douai", "54 avenue de Soissons BP 228 02401 CHATEAU THIERRY CEDEX" -> "54 avenue de
/// Soissons 02401 CHATEAU THIERRY". A box word (`query_box_words.tsv`, up to three words) goes only with the number
/// after it; "cedex" goes with the one- or two-digit office number after it. The postcode stays: it still names
/// the town (17.11.2026, French clean set: +11, −2, both losses the luck of a neighbour house).
pub fn without_postal_box(s: &str) -> std::borrow::Cow<'_, str> {
    // a box goes with its number, and CEDEX is the only mark dropped alone: no digit and no "cedex", nothing to do
    if !s.bytes().any(|b| b.is_ascii_digit()) && !s.to_lowercase().contains("cedex") {
        return std::borrow::Cow::Borrowed(s);
    }
    // tokens with their byte spans; commas and whitespace separate
    let mut toks: Vec<(usize, usize)> = Vec::new();
    let mut start = None;
    for (i, c) in s.char_indices() {
        if c.is_whitespace() || c == ',' {
            if let Some(b) = start.take() {
                toks.push((b, i));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(b) = start {
        toks.push((b, s.len()));
    }
    let key = |t: &str| normalize(t).replace(' ', "");
    let words: Vec<&str> = QUERY_BOX_WORDS
        .lines()
        .map(str::trim)
        .filter(|w| !w.is_empty() && !w.starts_with('#'))
        .collect();
    let number = |t: &str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit());
    let mut cut: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let text = |j: usize| &s[toks[j].0..toks[j].1];
        let mut taken = false;
        for n in (1..=3).rev() {
            if i + n < toks.len() {
                let glued: String = (i..i + n).map(|j| key(text(j))).collect();
                // a box that opens the query is its own form (`forms::special`: the town answers, `po_box`)
                if i > 0
                    && words.contains(&glued.as_str())
                    && number(text(i + n))
                    && !s[toks[i].0..toks[i + n].0].contains(',')
                {
                    cut.push((toks[i].0, toks[i + n].1));
                    i += n + 1;
                    taken = true;
                    break;
                }
            }
        }
        if taken {
            continue;
        }
        // a box word that is a whole segment by itself (", CS,": its number lost) goes too
        let alone = |a: usize, b: usize| {
            let before = s[..a].trim_end();
            let after = s[b..].trim_start();
            (before.is_empty() || before.ends_with(','))
                && (after.is_empty() || after.starts_with(','))
        };
        if i > 0 && words.contains(&key(text(i)).as_str()) && alone(toks[i].0, toks[i].1) {
            cut.push((toks[i].0, toks[i].1));
            i += 1;
            continue;
        }
        if key(text(i)) == "cedex" {
            // the office number stands in the same segment: "CEDEX 6", not "CEDEX, 58 RUE …" (a house number)
            let with_office = i + 1 < toks.len()
                && number(text(i + 1))
                && text(i + 1).len() <= 2
                && !s[toks[i].1..toks[i + 1].0].contains(',');
            let end = if with_office {
                toks[i + 1].1
            } else {
                toks[i].1
            };
            cut.push((toks[i].0, end));
            i += if with_office { 2 } else { 1 };
            continue;
        }
        i += 1;
    }
    if cut.is_empty() {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut at = 0;
    for (a, b) in cut {
        out.push_str(&s[at..a]);
        at = b;
    }
    out.push_str(&s[at..]);
    // the separators the cut left: ", ," / " - ," / doubled spaces / a dangling comma or dash at an end
    let mut tidy = out
        .split(',')
        .map(|seg| {
            seg.trim()
                .trim_end_matches(" -")
                .trim_start_matches("- ")
                .trim()
        })
        .filter(|seg| !seg.is_empty() && *seg != "-")
        .collect::<Vec<_>>()
        .join(", ");
    while tidy.contains("  ") {
        tidy = tidy.replace("  ", " ");
    }
    std::borrow::Cow::Owned(tidy)
}

pub fn normalize(s: &str) -> String {
    // Pre-pass: drop the hyphen in SINGLE-LETTER-digit block codes ("C-5" -> "c5");
    // otherwise the hyphen becomes a space and the digit is misread as a HOUSE NUMBER.
    // Applies ONLY when exactly one letter precedes the hyphen. Multi-letter names
    // ("Buz-1", "Dustlik-2") are NOT glued: there the digit is part of the name and
    // the index stores it with a space. Digit-hyphen-letter ordinals ("6-ya") are
    // left alone. A house-number RANGE ("15-17", digit-hyphen-digit) keeps the FIRST
    // number and drops the tail, so the two numbers do not compete.
    let pre: String = {
        let ch: Vec<char> = s.chars().collect();
        let mut o = String::with_capacity(s.len());
        let mut i = 0;
        while i < ch.len() {
            let c = ch[i];
            // number range "15-17": keep the first number, drop the hyphen and the second
            if c == '-'
                && i > 0
                && i + 1 < ch.len()
                && ch[i - 1].is_ascii_digit()
                && ch[i + 1].is_ascii_digit()
            {
                i += 1;
                while i < ch.len() && ch[i].is_ascii_digit() {
                    i += 1; // skip the second number of the range
                }
                continue;
            }
            // glue SINGLE letter + digit (block code like "C-5"): skip the hyphen
            if c == '-'
                && i > 0
                && i + 1 < ch.len()
                && ch[i - 1].is_alphabetic()
                && ch[i + 1].is_ascii_digit()
                && (i == 1 || !ch[i - 2].is_alphanumeric())
            {
                i += 1;
                continue;
            }
            o.push(c);
            i += 1;
        }
        o
    };
    let mut out = String::with_capacity(pre.len());
    let mut prev_space = true;
    for ch in pre.chars().flat_map(|c| c.to_lowercase()) {
        for d in ch.nfd() {
            if is_combining_mark(d) {
                continue; // stripped diacritic
            }
            let mapped: Option<char> = match d {
                '-' | '\'' | '’' | '‘' | 'ʻ' | 'ʼ' | '`' | '.' | '/' | ',' | ';' | '(' | ')' => {
                    None
                }
                c if c.is_whitespace() => None,
                // đ (U+0111) does not decompose under NFD; fold it to "d" on BOTH sides
                // (the index build does the same). Input forms đ / dj / d then converge
                // (Karađorđeva = Karadjordjeva = Karadordeva -> "karadordeva");
                // the dj -> d fold itself is handled in query.rs.
                '\u{0111}' => Some('d'),
                // junk characters: emoji/flags/arrows/invisibles (ZWSP, bidi controls,
                // variation selectors) become separators — a trailing flag emoji or a
                // ZWSP inside a word would otherwise yield no match. The numero sign
                // (U+2116) is kept: it occurs in Russian street names.
                '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{FE00}'..='\u{FE0F}'
                | '\u{2190}'..='\u{2BFF}'
                | '\u{1F000}'..='\u{1FFFF}' => None,
                c if c.is_control() => None,
                c => Some(c),
            };
            match mapped {
                Some(c) => {
                    out.push(c);
                    prev_space = false;
                }
                None => {
                    if !prev_space {
                        out.push(' ');
                        prev_space = true;
                    }
                }
            }
        }
    }
    let mut out = split_glued_postcode(&out);
    while out.ends_with(' ') {
        out.pop();
    }
    out
}

/// Split a glued "postcode+city" seam: a run of >=4 digits immediately followed by
/// a letter gets a space inserted ("75001paris" -> "75001 paris", "9402assen" -> ...).
/// House numbers with a letter suffix ("12a") have digit runs shorter than 4 and are
/// left alone. A real class of human input: the postcode pasted flush against the city.
fn is_glued_postcode_boundary(digit_run: usize, character: char, output_ends_space: bool) -> bool {
    digit_run >= 4 && character.is_alphabetic() && !output_ends_space
}

fn split_glued_postcode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    let mut digit_run = 0usize;
    for c in s.chars() {
        if c.is_ascii_digit() {
            digit_run += 1;
        } else {
            if is_glued_postcode_boundary(digit_run, c, out.ends_with(' ')) {
                out.push(' ');
            }
            digit_run = 0;
        }
        out.push(c);
    }
    out
}

/// Test-observer for the exact seam predicate used above.  The returned
/// value is the real normalized output, not a regex approximation.
#[cfg(test)]
pub(crate) fn de_postcode_seam_trace(raw: &str) -> Option<String> {
    let mut digit_run = 0usize;
    let mut ends_space = true;
    for character in raw.chars() {
        if character.is_ascii_digit() {
            digit_run += 1;
        } else {
            if is_glued_postcode_boundary(digit_run, character, ends_space) {
                return Some(format!("{raw} -> {}", normalize(raw)));
            }
            digit_run = 0;
        }
        ends_space = character.is_whitespace();
    }
    None
}

/// Cyrillic → Gaj's Latin alphabet (the Serbian script pair, 1:1) plus an
/// approximation for Russian letters outside that set (ya→ja, yu→ju, shcha→šč,
/// soft/hard signs dropped). Input is an already-normalized string (yo/short-i
/// were folded via NFD). The output is run through normalize() again so that
/// ž/š/č/ć land exactly as in the index.
pub fn translit_cyr_lat(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for c in s.chars() {
        match c {
            'а' => out.push('a'),
            'б' => out.push('b'),
            'в' => out.push('v'),
            'г' => out.push('g'),
            'д' => out.push('d'),
            'ђ' => out.push('đ'),
            'е' => out.push('e'),
            'ж' => out.push('ž'),
            'з' => out.push('z'),
            'и' => out.push('i'),
            'ј' => out.push('j'),
            'к' => out.push('k'),
            'л' => out.push('l'),
            'љ' => out.push_str("lj"),
            'м' => out.push('m'),
            'н' => out.push('n'),
            'њ' => out.push_str("nj"),
            'о' => out.push('o'),
            'п' => out.push('p'),
            'р' => out.push('r'),
            'с' => out.push('s'),
            'т' => out.push('t'),
            'ћ' => out.push('ć'),
            'у' => out.push('u'),
            'ф' => out.push('f'),
            'х' => out.push('h'),
            'ц' => out.push('c'),
            'ч' => out.push('č'),
            'џ' => out.push_str("dž"),
            'ш' => out.push('š'),
            // Russian letters outside the Serbian set — approximate
            'я' => out.push_str("ja"),
            'ю' => out.push_str("ju"),
            'щ' => out.push_str("šč"),
            'ы' => out.push('i'),
            'э' => out.push('e'),
            'ь' | 'ъ' => {}
            other => out.push(other),
        }
    }
    out
}

/// Cyrillic → Latin using ENGLISH DIGRAPHS (ch, sh, zh, ya, y...).
/// translit_cyr_lat above yields Serbian Gaj Latin with diacritics (č, š), which
/// does NOT match data romanized the English way (e.g. Overture/OSM Uzbekistan:
/// "Chilanzar", "Yakkasaray", "Toshkent"). This variant targets such data and
/// works for any language whose Cyrillic is romanized with English digraphs.
pub fn translit_cyr_lat_en(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for c in s.chars() {
        match c {
            'а' => out.push('a'),
            'б' => out.push('b'),
            'в' => out.push('v'),
            'г' => out.push('g'),
            'д' => out.push('d'),
            'е' | 'э' => out.push('e'),
            'ё' => out.push_str("yo"),
            'ж' => out.push_str("zh"),
            'з' => out.push('z'),
            'и' | 'ы' => out.push('i'),
            'й' => out.push('y'), // word-final -ay/-oy endings (Yakkasaray, Tolstoy)
            'к' => out.push('k'),
            'л' => out.push('l'),
            'м' => out.push('m'),
            'н' => out.push('n'),
            'о' => out.push('o'),
            'п' => out.push('p'),
            'р' => out.push('r'),
            'с' => out.push('s'),
            'т' => out.push('t'),
            'у' => out.push('u'),
            'ф' => out.push('f'),
            'х' => out.push('h'),
            'ц' => out.push_str("ts"),
            'ч' => out.push_str("ch"),
            'ш' => out.push_str("sh"),
            'щ' => out.push_str("shch"),
            'ю' => out.push_str("yu"),
            'я' => out.push_str("ya"),
            // Uzbek Cyrillic letters -> Uzbek Latin (normalize strips the apostrophe:
            // o'/g' -> o/g)
            'ў' => out.push('o'),
            'ғ' => out.push('g'),
            'қ' => out.push('q'),
            'ҳ' => out.push('h'),
            'ҷ' => out.push('j'),
            'ь' | 'ъ' => {}
            other => out.push(other),
        }
    }
    out
}

/// whether the string contains Cyrillic
pub fn has_cyrillic(s: &str) -> bool {
    s.chars().any(|c| ('\u{0400}'..='\u{04FF}').contains(&c))
}

/// whether the string contains Latin letters
pub fn has_latin(s: &str) -> bool {
    s.chars().any(|c| c.is_ascii_alphabetic())
}

/// Reusable skeleton for phonetic folding. Groups of equivalent letters map to one
/// canonical char, so spellings of a name across languages/scripts yield ONE key
/// (Russian "Kadyri" vs Uzbek "Qodiriy" -> both "kodiri"). To support a new
/// language, add a "variants -> canon" row. The current set is Turkic (Uzbek):
/// back vowels a/o/u; front vowels e/i/y; q/k; x/h; w/v.
const PHON_GROUPS: &[(&str, char)] = &[
    ("aou", 'o'), // back vowels: Russian "a" in closed syllables matches Uzbek "o"/"u"
    ("eiy", 'i'), // front vowels + glide
    ("qk", 'k'),
    ("xh", 'h'),
    ("wv", 'v'), // consonant variants
];

/// Phonetic key of a name — canonical form across per-language spelling variants.
/// Fold to Latin, collapse PHON_GROUPS, drop doubled letters. An ADDITIONAL key for
/// fuzzy (subset) matching, not a replacement for exact matching. Empty for short or
/// non-romanizable input. New languages plug in via new groups.
pub fn phonetic_key(s: &str) -> String {
    let lat = if has_cyrillic(s) {
        translit_cyr_lat_en(s)
    } else {
        s.to_string()
    };
    let n = normalize(&lat);
    let mut out = String::with_capacity(n.len());
    let mut prev = '\0';
    for ch in n.chars() {
        if ch == ' ' {
            out.push(' ');
            prev = '\0';
            continue;
        }
        let c = PHON_GROUPS
            .iter()
            .find(|(set, _)| set.contains(ch))
            .map(|(_, k)| *k)
            .unwrap_or(ch);
        if c != prev {
            out.push(c); // collapse doubled letters (qq→q, oo→o)
            prev = c;
        }
    }
    out
}

/// Latin→Cyrillic homoglyph folding. Copy-paste and keyboard-layout slips inject
/// STRAY Latin letters into a Cyrillic word (e.g. Latin e and a inside "Tverskaya"
/// typed in Cyrillic). Visually identical Latin letters (a/b/c/e/h/k/m/o/p/t/x/y)
/// are folded to Cyrillic — but ONLY in a MIXED token where Cyrillic outnumbers
/// Latin. Pure-Latin tokens ("Tverskaya") and Latin-majority tokens are untouched.
/// This map is VISUAL (by letter shape), not phonetic (cf. translit_lat_cyr,
/// which maps by sound). Query-side only.
pub fn fold_homoglyphs(s: &str) -> String {
    fn homoglyph(c: char) -> Option<char> {
        Some(match c {
            'a' => 'а',
            'b' => 'в',
            'c' => 'с',
            'e' => 'е',
            'h' => 'н',
            'k' => 'к',
            'm' => 'м',
            'o' => 'о',
            'p' => 'р',
            't' => 'т',
            'x' => 'х',
            'y' => 'у',
            _ => return None,
        })
    }
    if !s.chars().any(|c| c.is_ascii_alphabetic()) || !has_cyrillic(s) {
        return s.to_string(); // no script mix — nothing to fold
    }
    s.split(' ')
        .map(|tok| {
            let cyr = tok
                .chars()
                .filter(|c| ('\u{0400}'..='\u{04FF}').contains(c))
                .count();
            let lat = tok.chars().filter(|c| c.is_ascii_alphabetic()).count();
            // only mixed tokens with a Cyrillic majority (pure Latin is untouched)
            if cyr == 0 || lat == 0 || cyr <= lat {
                return tok.to_string();
            }
            tok.chars().map(|c| homoglyph(c).unwrap_or(c)).collect()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Latin → Cyrillic (approximate): Russian/Uzbek addresses typed in Latin against
/// Cyrillic data ("bratsk mira 60" with the city stored in Cyrillic). Greedy over
/// UNAMBIGUOUS digraphs; "ts" is NOT a digraph (so "bratsk" keeps t+s). Cyrillic,
/// digits and spaces pass through unchanged. Used as a FALLBACK pass when the
/// result is empty; the output is run through normalize() again (yo/short-i fold
/// as in the index).
pub fn translit_lat_cyr(s: &str) -> String {
    let ch: Vec<char> = s.chars().collect();
    let n = ch.len();
    let starts = |i: usize, p: &str| -> bool {
        let pc: Vec<char> = p.chars().collect();
        i + pc.len() <= n && ch[i..i + pc.len()] == pc[..]
    };
    let mut out = String::with_capacity(s.len() * 2);
    let mut i = 0;
    while i < n {
        if starts(i, "shch") {
            out.push('щ');
            i += 4;
        } else if starts(i, "sch") {
            out.push('щ');
            i += 3;
        } else if starts(i, "zh") {
            out.push('ж');
            i += 2;
        } else if starts(i, "kh") {
            out.push('х');
            i += 2;
        } else if starts(i, "ch") {
            out.push('ч');
            i += 2;
        } else if starts(i, "sh") {
            out.push('ш');
            i += 2;
        } else if starts(i, "yo") {
            out.push('ё');
            i += 2;
        } else if starts(i, "yu") {
            out.push('ю');
            i += 2;
        } else if starts(i, "ya") {
            out.push('я');
            i += 2;
        } else if ch[i] == 'y' {
            // "y" is ambiguous: after a VOWEL it is the glide (-skiy/-oy endings),
            // after a consonant or word-initially it is the vowel (as in "Krym").
            // Decided by the last character already emitted.
            let prev_vowel = matches!(
                out.chars().last(),
                Some('а' | 'е' | 'ё' | 'и' | 'о' | 'у' | 'ы' | 'э' | 'ю' | 'я')
            );
            out.push(if prev_vowel { 'й' } else { 'ы' });
            i += 1;
        } else {
            out.push(match ch[i] {
                'a' => 'а',
                'b' => 'б',
                'c' => 'к',
                'd' => 'д',
                'e' => 'е',
                'f' => 'ф',
                'g' => 'г',
                'h' => 'х',
                'i' => 'и',
                'j' => 'й',
                'k' => 'к',
                'l' => 'л',
                'm' => 'м',
                'n' => 'н',
                'o' => 'о',
                'p' => 'п',
                'q' => 'к',
                'r' => 'р',
                's' => 'с',
                't' => 'т',
                'u' => 'у',
                'v' => 'в',
                'w' => 'в',
                'x' => 'х',
                'z' => 'з',
                other => other,
            });
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_line_loses_the_notes_about_its_place() {
        use super::without_place_notes as w;
        assert_eq!(
            w("2 N. 5th St., Richmond (Independent City), VIRGINIA").as_deref(),
            Some("2 N. 5th St., Richmond, VIRGINIA")
        );
        assert_eq!(
            w("1511 (rear) Twenty-second St. NW, Washington").as_deref(),
            Some("1511 Twenty-second St. NW, Washington")
        );
        assert_eq!(
            w("416 S. Broadway, Borough of West Cape May, NEW JERSEY").as_deref(),
            Some("416 S. Broadway, West Cape May, NEW JERSEY")
        );
        assert_eq!(
            w("394 93rd St., Stone Harbor Borough, NEW JERSEY").as_deref(),
            Some("394 93rd St., Stone Harbor, NEW JERSEY")
        );
        // nothing to drop; an unclosed parenthesis is no note; a street called "Borough" stays
        assert_eq!(w("2 Main St., Cape May"), None);
        assert_eq!(w("2 Main St. (rear, Cape May"), None);
        // a parenthetical that carries the address or the place stays
        assert_eq!(w("LASALLE, QC (7400 ST-PATRICK)"), None);
        assert_eq!(
            w("Nummer 11A, Ypekolsgea (Súdwest-Fryslân), Friesland"),
            None
        );
        assert_eq!(w("Smythe Road Perth (West-Australië)"), None);
        assert_eq!(w("12 Borough High Street, London"), None);
        assert_eq!(w("12 Old Borough, London"), None);
        assert_eq!(w("12 Borough, London"), None);
        assert_eq!(w("(Independent City)"), None);
    }

    #[test]
    fn a_line_writes_its_spelled_ordinals_as_numbers() {
        use super::with_numbered_ordinals as o;
        assert_eq!(
            o("620 S. Eighth St., Sheboygan").as_deref(),
            Some("620 S. 8th St., Sheboygan")
        );
        assert_eq!(
            o("461 Twenty-seventh St., Ogden").as_deref(),
            Some("461 27th St., Ogden")
        );
        assert_eq!(o("645 W. Third Ave.").as_deref(), Some("645 W. 3rd Ave."));
        assert_eq!(
            o("601 West Second Street").as_deref(),
            Some("601 West 2nd Street")
        );
        assert_eq!(
            o("1 First, 2 Twelfth, 3 Thirteenth").as_deref(),
            Some("1 1st, 2 12th, 3 13th")
        );
        assert_eq!(o("Twenty-first Ave").as_deref(), Some("21st Ave"));
        assert_eq!(o("Twenty-second St").as_deref(), Some("22nd St"));
        assert_eq!(o("Thirty-third St").as_deref(), Some("33rd St"));
        assert_eq!(o("Twentieth St").as_deref(), Some("20th St"));
        // no ordinal; a tens word alone, or with a non-unit, is not one
        assert_eq!(o("2 Main St., Cape May"), None);
        assert_eq!(o("Twenty Oaks Rd"), None);
        assert_eq!(o("Twenty-twelfth St"), None);
        assert_eq!(o("Firsthand Rd"), None);
    }

    #[test]
    fn the_addresses_a_line_lists_each_take_the_town_of_its_tail() {
        use super::listed_addresses as l;
        assert_eq!(
            l("1 quai Bourbon ; 3 rue des Deux-Ponts, Paris 4e Arrondissement"),
            [
                "1 quai Bourbon, Paris 4e Arrondissement",
                "3 rue des Deux-Ponts, Paris 4e Arrondissement"
            ]
        );
        assert_eq!(
            l("26 rue des Tanneries ; 21 quai des Cordeliers ; 47 rue Porte-aux-Saints, Mantes"),
            [
                "26 rue des Tanneries, Mantes",
                "21 quai des Cordeliers, Mantes",
                "47 rue Porte-aux-Saints, Mantes"
            ]
        );
        // no town in the tail: the doors are not read apart (each would be read anywhere)
        assert!(l("rue X ; rue Y").is_empty());
        assert!(l("400 Rusk Street; Houston TX 77002").is_empty());
        // the ";" of an HTML entity separates nothing
        assert!(l("aan het einde van&nbsp;de&nbsp;Heirweg").is_empty());
        assert_eq!(
            l("1 rue A&amp;B ; 2 rue C, Lyon"),
            ["1 rue A B, Lyon", "2 rue C, Lyon"]
        );
        assert!(l("12 rue du Bac, Paris").is_empty());
        assert_eq!(l("5 rue X ; , Lyon"), ["5 rue X, Lyon"]);
        assert_eq!(
            l("AT&T Plaza 1 ; 2 rue C, Lyon"),
            ["AT&T Plaza 1, Lyon", "2 rue C, Lyon"]
        );
    }

    #[test]
    fn a_line_break_tag_is_a_separator() {
        use super::without_line_break_tags as w;
        assert_eq!(
            w("Place de la Gare<br />67630 Lauterbourg"),
            "Place de la Gare, 67630 Lauterbourg"
        );
        assert_eq!(w("Rue X<BR>Paris"), "Rue X, Paris");
        assert_eq!(w("Rue X<br/>Paris"), "Rue X, Paris");
        assert_eq!(w("Rue <b>X</b>, Paris"), "Rue <b>X</b>, Paris");
        assert_eq!(w("Rue X <brique>"), "Rue X <brique>");
        assert_eq!(w("Résidence <br />Les Pins"), "Résidence , Les Pins");
    }

    #[test]
    fn a_post_office_box_and_the_cedex_mark_leave_the_query() {
        use super::without_postal_box as w;
        assert_eq!(
            w("246 rue Saint Jean BP 70639 59500 Douai"),
            "246 rue Saint Jean 59500 Douai"
        );
        assert_eq!(
            w("10-12 avenue Foch - BP 70330,57016,Metz Cedex 1"),
            "10-12 avenue Foch, 57016, Metz"
        );
        assert_eq!(
            w("1517 PL DE L HOTEL DE VILLE, CS 40051, 76084 LE HAVRE CEDEX"),
            "1517 PL DE L HOTEL DE VILLE, 76084 LE HAVRE"
        );
        assert_eq!(
            w("Musterstraße 5, Postfach 1234, 10115 Berlin"),
            "Musterstraße 5, 10115 Berlin"
        );
        assert_eq!(w("1 rue X, B.P. 12, 75001 Paris"), "1 rue X, 75001 Paris");
        // a box that opens the query stays: it is its own form, answered by the town (forms::special)
        assert_eq!(
            w("Boîte postale 7, 1 rue X, Lyon"),
            "Boîte postale 7, 1 rue X, Lyon"
        );
        assert_eq!(
            w("PO Box 12, Main Street 5, Ville"),
            "PO Box 12, Main Street 5, Ville"
        );
        // a box word without its number, a Spanish "CP" postcode label and a name word stay
        assert_eq!(w("Rue du BP, Paris"), "Rue du BP, Paris");
        // with digits elsewhere in the line the box word still needs its own number
        assert_eq!(
            w("Station BP Total, 12 rue X, Lyon"),
            "Station BP Total, 12 rue X, Lyon"
        );
        assert_eq!(
            w("Calle Mayor 5, CP 28013 Madrid"),
            "Calle Mayor 5, CP 28013 Madrid"
        );
        assert_eq!(
            w("Box Hill Road 3, Melbourne"),
            "Box Hill Road 3, Melbourne"
        );
        assert_eq!(w("Rue Cedexis 4, Lyon 3e"), "Rue Cedexis 4, Lyon 3e");
        // CEDEX takes an office number of one or two digits, never a postcode
        assert_eq!(
            w("1 place X, 31040 TOULOUSE CEDEX 6"),
            "1 place X, 31040 TOULOUSE"
        );
        assert_eq!(
            w("1 place X, TOULOUSE CEDEX 31040"),
            "1 place X, TOULOUSE 31040"
        );
        assert_eq!(w("12 rue du Bac, Paris"), "12 rue du Bac, Paris");
        // the office number is in the CEDEX segment; a number after a comma is the next address part
        assert_eq!(
            w("70014 VESOUL CEDEX, 58 RUE PAUL MOREL, BP 392"),
            "70014 VESOUL, 58 RUE PAUL MOREL"
        );
        // a box word left alone as a segment (its number lost) goes; inside a segment it stays
        assert_eq!(
            w("1517 PL DE L HOTEL DE VILLE, CS, 76084 LE HAVRE CEDEX"),
            "1517 PL DE L HOTEL DE VILLE, 76084 LE HAVRE"
        );
    }

    #[test]
    fn typographic_signs_become_the_ascii_ones_and_nothing_else_changes() {
        use super::ascii_signs;
        assert_eq!(
            ascii_signs("Unter den Linden 4 \u{2013} 10117 Berlin"),
            "Unter den Linden 4 - 10117 Berlin"
        );
        // a dash inside a range or a word stays: a range written with it is read as before
        assert_eq!(
            ascii_signs("Rathausplatz 2\u{2013}4"),
            "Rathausplatz 2\u{2013}4"
        );
        // two numbers around a spaced dash are a range, as the number reader knows it; elsewhere a separator
        assert_eq!(
            ascii_signs("Stari\u{2014}Grad 40 \u{2014} 44"),
            "Stari\u{2014}Grad 40-44"
        );
        assert_eq!(
            ascii_signs("Normannenweg 17 \u{2013} 21, 20357 Hamburg"),
            "Normannenweg 17-21, 20357 Hamburg"
        );
        assert_eq!(
            ascii_signs("Opferperspektive \u{2013} e.V."),
            "Opferperspektive - e.V."
        );
        assert_eq!(
            ascii_signs("Rua Garcia D\u{00B4}Avila 105"),
            "Rua Garcia D'Avila 105"
        );
        assert_eq!(ascii_signs("O\u{2019}Brien Street"), "O'Brien Street");
        // double quotes stay: sheet keys keep „…” and "…" as their sources wrote them
        assert_eq!(
            ascii_signs("Aleja \u{201E}Solidarności\u{201D} 58"),
            "Aleja \u{201E}Solidarności\u{201D} 58"
        );
        assert_eq!(
            ascii_signs("\u{00AB}Le Cèdre\u{00BB}"),
            "\u{00AB}Le Cèdre\u{00BB}"
        );
        assert_eq!(
            ascii_signs("Calle \u{201C}Real\u{201D} 1"),
            "Calle \u{201C}Real\u{201D} 1"
        );
        assert!(matches!(
            ascii_signs("Rue de la Paix 10"),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    use super::normalize;

    #[test]
    fn mirrors_duckdb() {
        assert_eq!(
            normalize("Avenue du Général-Leclerc"),
            "avenue du general leclerc"
        );
        assert_eq!(normalize("Rue de l'Église"), "rue de l eglise");
        assert_eq!(normalize("Bulevar Oslobođenja"), "bulevar oslobodenja"); // đ→d on both sides
        assert_eq!(normalize("Šafarikova ČĆŽ"), "safarikova ccz");
        assert_eq!(normalize("Ёлки-Йод"), "елки иод"); // yo→e, short i→i via NFD
        assert_eq!(normalize("oʻzbekiston g‘afur"), "o zbekiston g afur");
    }

    #[test]
    fn translit_serbian_pair() {
        use super::translit_cyr_lat;
        // the Serbian script pair 1:1 + diacritics stripped exactly as in the index
        assert_eq!(
            normalize(&translit_cyr_lat("милентија поповића")),
            "milentija popovica"
        );
        // a Russian spelling of a Serbian address converges to the same form
        assert_eq!(
            normalize(&translit_cyr_lat("милентия поповича")),
            "milentija popovica"
        );
        assert_eq!(
            normalize(&translit_cyr_lat("кнеза михаила")),
            "kneza mihaila"
        );
    }

    #[test]
    fn translit_latin_to_cyrillic() {
        use super::translit_lat_cyr;
        // "ts" is not a digraph: "bratsk" keeps t+s
        assert_eq!(normalize(&translit_lat_cyr("bratsk")), "братск");
        assert_eq!(normalize(&translit_lat_cyr("tashkent")), "ташкент");
        // digraphs and adjacent Cyrillic are left intact
        assert_eq!(
            normalize(&translit_lat_cyr("улица возрождения 1 bratsk")),
            "улица возрождения 1 братск"
        );
        // "y" is context-dependent: after a vowel -> glide (-skiy/-oy/-yy endings),
        // after a consonant -> vowel. normalize then folds the glide to plain i
        // (as in the index and DuckDB), so these forms match the registry.
        assert_eq!(normalize(&translit_lat_cyr("leninskiy")), "ленинскии");
        assert_eq!(
            normalize(&translit_lat_cyr("novoyasenevskiy")),
            "новоясеневскии"
        );
        assert_eq!(normalize(&translit_lat_cyr("tolstoy")), "толстои");
        assert_eq!(normalize(&translit_lat_cyr("krym")), "крым"); // y after a consonant -> vowel
    }

    #[test]
    fn fold_homoglyphs_mixed_script() {
        use super::fold_homoglyphs;
        // Latin e (U+0065) and a (U+0061) inside a Cyrillic word fold to Cyrillic
        assert_eq!(
            fold_homoglyphs("тв\u{0065}рск\u{0061}я 6 москва"),
            "тверская 6 москва"
        );
        // pure Latin is NOT touched (a genuine Latin query)
        assert_eq!(fold_homoglyphs("tverskaya 6 moskva"), "tverskaya 6 moskva");
        // pure Cyrillic — unchanged
        assert_eq!(fold_homoglyphs("тверская 6 москва"), "тверская 6 москва");
        // Latin-majority token is untouched (not "Cyrillic with a slip")
        assert_eq!(fold_homoglyphs("ab\u{0432}"), "ab\u{0432}"); // 2 Latin, 1 Cyrillic -> no fold
                                                                 // visual map, not phonetic; Cyrillic-majority token
        assert_eq!(
            fold_homoglyphs("\u{0079}\u{0062}\u{043E}\u{0440}\u{043A}\u{0430}"),
            "уворка"
        );
    }
}
