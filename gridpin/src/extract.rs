//! Addresses in free text, confirmed by the engine itself (`gridpin extract`).
//!
//! No model: every candidate is a window of 2 to 10 words of one line (a line break or ";" ends it) that holds an anchor (a house
//! number of 1-5 digits or a street-type word of the sheet's own rules). The window is asked of the engine as
//! a query; it is kept only when the answer is a house whose number is the asked one, or a street the query
//! named. Of overlapping windows the longest house wins, then the longest street. A phone number ("+381 11
//! 1234567") is never an anchor and no window may hold one.
//!
//! A window is a cut of its line, and the cut may drop the place the line names. An answer that did not take its
//! place from the window itself is therefore checked against the line: the postcode the engine reads in the
//! longest window starting there, and a commune of the sheet written right after the window or after that
//! postcode. An answer in another place ("20 rue de la République" of "… 13002 Marseille" answered in Troyes),
//! or on a street named by its type word alone ("20 rue" → the street "Rue" of a village), is neither found nor
//! reported.
//!
//! `verify` also reports the windows with a number that the engine does NOT confirm as a house, with a
//! reason read from the answer (`street_unknown`, `number_not_on_street`, `ambiguous`): the addresses a text
//! (an answer of a language model) names but the sheet does not have.

use serde::Serialize;

use crate::norm::normalize;
use crate::query::{query_cascade, Hit, Index};
use crate::verdict;

/// At most this many windows are asked per document: a text without addresses stays cheap.
pub const MAX_WINDOWS: usize = 2000;
const MIN_WORDS: usize = 2;
const MAX_WORDS: usize = 10;

/// One address found in the text. `start`/`end` are byte offsets into the text as given.
#[derive(Clone, Serialize)]
pub struct Found {
    pub start: usize,
    pub end: usize,
    pub text: String,
    /// `house` or `street`; `none` for a `verify` report of an address the sheet does not have
    pub level: &'static str,
    /// `found`, or `not_found` (only with `verify`)
    pub verdict: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hit: Option<Hit>,
}

/// What one call did, for the log on stderr.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub windows: usize,
    pub capped: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Word,
    Number,
    Phone,
}

struct Token {
    start: usize,
    end: usize,
    kind: Kind,
    line: usize,
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '\'' | '’' | '-' | '/' | '.')
}

/// A house number: 1-5 digits, optionally with one letter or a `/n` tail ("12", "12a", "12/3").
fn is_house_number(s: &str) -> bool {
    let digits = s.chars().take_while(char::is_ascii_digit).count();
    if !(1..=5).contains(&digits) {
        return false;
    }
    let rest = &s[digits..];
    rest.is_empty()
        || (rest.chars().count() == 1 && rest.chars().all(char::is_alphabetic))
        || (rest.starts_with('/')
            && rest[1..].chars().all(|c| c.is_ascii_digit())
            && rest.len() <= 4)
}

fn tokens(text: &str) -> Vec<Token> {
    let mut out: Vec<Token> = Vec::new();
    let mut line = 0;
    let mut cur: Option<usize> = None;
    let push = |out: &mut Vec<Token>, start: usize, end: usize, line: usize| {
        let raw = text[start..end].trim_end_matches(['.', '-', '/', '\'']);
        let end = start + raw.len();
        if raw.is_empty() {
            return;
        }
        // a long digit run, or digits joined by hyphens ("222-33-44"), reads as part of a phone
        let digit_count = raw.chars().filter(char::is_ascii_digit).count();
        let dialled = raw.chars().all(|c| c.is_ascii_digit() || c == '-') && digit_count >= 6;
        let kind = if dialled || (raw.chars().all(|c| c.is_ascii_digit()) && raw.len() > 5) {
            Kind::Phone
        } else if is_house_number(raw) {
            Kind::Number
        } else {
            Kind::Word
        };
        out.push(Token {
            start,
            end,
            kind,
            line,
        });
    };
    for (i, c) in text.char_indices() {
        if is_word_char(c) {
            cur.get_or_insert(i);
        } else {
            if let Some(s) = cur.take() {
                push(&mut out, s, i, line);
            }
            // ";" ends an item of a list as a line break does: "10 rue Haute ; Cite, 14 rue Haute" is two
            if c == '\n' || c == ';' {
                line += 1;
            }
        }
    }
    if let Some(s) = cur {
        push(&mut out, s, text.len(), line);
    }
    mark_phones(text, &mut out);
    out
}

/// Runs of number tokens that together read as a phone: led by `+`, or seven digits or more in all.
fn mark_phones(text: &str, toks: &mut [Token]) {
    let numeric = |t: &Token| {
        t.kind != Kind::Word
            && text[t.start..t.end]
                .chars()
                .all(|c| c.is_ascii_digit() || c == '-')
    };
    let mut i = 0;
    while i < toks.len() {
        if !numeric(&toks[i]) {
            i += 1;
            continue;
        }
        let mut j = i;
        while j + 1 < toks.len()
            && numeric(&toks[j + 1])
            && toks[j + 1].line == toks[i].line
            && text[toks[j].end..toks[j + 1].start]
                .chars()
                .all(|c| matches!(c, ' ' | '-' | '(' | ')'))
        {
            j += 1;
        }
        let digits: usize = toks[i..=j]
            .iter()
            .map(|t| {
                text[t.start..t.end]
                    .chars()
                    .filter(char::is_ascii_digit)
                    .count()
            })
            .sum();
        let plus = text[..toks[i].start]
            .trim_end_matches([' ', '('])
            .ends_with('+');
        if plus || digits >= 7 || toks[i..=j].iter().any(|t| t.kind == Kind::Phone) {
            for t in &mut toks[i..=j] {
                t.kind = Kind::Phone;
            }
        }
        i = j + 1;
    }
}

struct Candidate {
    start: usize,
    end: usize,
    words: usize,
    house: bool,
    /// the street was found as written (`street_exact`), not one letter off
    exact: bool,
    /// the engine read the whole window (no `dropped_*` flag): preferred over a longer window it trimmed
    clean: bool,
    /// the window starts at its number or a street-type word, where an address starts
    lead: bool,
    /// the window holds a house number
    number: bool,
    found: Found,
}

/// The windows of one line that hold an anchor and no phone, longest first is not required: all of them.
fn windows(idx: &Index, text: &str, toks: &[Token]) -> Vec<(usize, usize, bool)> {
    let anchor = |t: &Token| is_anchor(idx, text, t);
    let mut out = Vec::new();
    for i in 0..toks.len() {
        for n in MIN_WORDS..=MAX_WORDS {
            let j = i + n - 1;
            if j >= toks.len() || toks[j].line != toks[i].line {
                break;
            }
            let span = &toks[i..=j];
            if span.iter().any(|t| t.kind == Kind::Phone) {
                break;
            }
            if span.iter().any(anchor) {
                out.push((i, j, span.iter().any(|t| t.kind == Kind::Number)));
            }
        }
    }
    out
}

fn is_anchor(idx: &Index, text: &str, t: &Token) -> bool {
    t.kind == Kind::Number || idx.is_street_type(&normalize(&text[t.start..t.end]))
}

/// Why the engine did not confirm a window with a number as a house.
fn reason(hits: &[Hit]) -> &'static str {
    let Some(h) = hits.first() else {
        return "street_unknown";
    };
    if hits.len() > 1 && hits[1].commune != h.commune && hits[1].street == h.street {
        return "ambiguous";
    }
    if h.flags.contains(&"ambiguous_far") {
        return "ambiguous";
    }
    if !h.street.is_empty() && matches!(h.precision, "street" | "interp" | "near" | "house") {
        "number_not_on_street"
    } else {
        "street_unknown"
    }
}

/// Addresses in `text`. With `verify`, also the windows with a number the sheet does not confirm.
pub fn extract(idx: &Index, text: &str, verify: bool) -> (Vec<Found>, Stats) {
    let toks = tokens(text);
    let mut all = windows(idx, text, &toks);
    let stats = Stats {
        windows: all.len().min(MAX_WINDOWS),
        capped: all.len() > MAX_WINDOWS,
    };
    all.truncate(MAX_WINDOWS);
    use rayon::prelude::*;
    let judged: Vec<(Candidate, bool)> = all
        .into_par_iter()
        .filter_map(|(i, j, has_number)| {
            let (start, end) = (toks[i].start, toks[j].end);
            let window = &text[start..end];
            let hits = query_cascade(idx, None, window, if verify { 2 } else { 1 });
            let level = match hits.first().map(|h| judge(idx, text, &toks, (i, j), h)) {
                Some(Judged::Found(level)) => Some(level),
                // an answer from another place than the line names is no report about this address either
                Some(Judged::Elsewhere) => return None,
                Some(Judged::Missing) | None => None,
            };
            if level.is_none() && !(verify && has_number) {
                return None;
            }
            let exact = hits
                .first()
                .is_some_and(|h| h.flags.contains(&"street_exact"));
            // an answer that is only a place ("13002 Marseille") read no street: it is no cleaner than no answer
            let clean = hits.first().is_some_and(|h| {
                h.precision != "city" && !h.flags.iter().any(|f| f.starts_with("dropped_"))
            });
            let found = Found {
                start,
                end,
                text: window.to_string(),
                level: level.unwrap_or("none"),
                verdict: if level.is_some() {
                    "found"
                } else {
                    "not_found"
                },
                reason: level.is_none().then(|| reason(&hits)),
                hit: hits.first().cloned(),
            };
            let c = Candidate {
                start,
                end,
                words: j - i + 1,
                house: level == Some("house"),
                exact,
                clean,
                lead: is_anchor(idx, text, &toks[i]),
                number: has_number,
                found,
            };
            Some((c, level.is_some()))
        })
        .collect();
    let (found, missing): (Vec<_>, Vec<_>) = judged.into_iter().partition(|(_, ok)| *ok);
    let found: Vec<Candidate> = found.into_iter().map(|(c, _)| c).collect();
    let missing: Vec<Candidate> = missing.into_iter().map(|(c, _)| c).collect();
    let mut out = pick(found, false);
    if verify {
        // a number window is reported missing where no house was found over it; a street found over it is
        // replaced by the report: the text named a number on that street, and the sheet does not have it
        let overlaps = |a: &Candidate, b: &Candidate| a.start < b.end && b.start < a.end;
        let mut missing: Vec<Candidate> = missing
            .into_iter()
            .filter(|m| !out.iter().any(|f| f.house && overlaps(f, m)))
            .collect();
        // the street found over a number is itself the report: its own words, its own answer
        let (replaced, kept): (Vec<Candidate>, Vec<Candidate>) = out
            .into_iter()
            .partition(|f| !f.house && f.number && missing.iter().any(|m| overlaps(f, m)));
        out = kept;
        missing.extend(replaced.into_iter().map(|mut f| {
            f.found.level = "none";
            f.found.verdict = "not_found";
            f.found.reason = Some("number_not_on_street");
            f
        }));
        // a report names the address from where it starts, not from the words before it
        let missing = pick(missing, true);
        out.retain(|f| f.house || !missing.iter().any(|m| overlaps(f, m)));
        out.extend(missing);
    }
    out.sort_by_key(|c| c.start);
    (out.into_iter().map(|c| c.found).collect(), stats)
}

enum Judged {
    /// an address of the text, at this level
    Found(&'static str),
    /// not confirmed (with `verify`, a window with a number is reported)
    Missing,
    /// an answer in another place than the text names, or a street named by its type word alone: never found,
    /// never reported
    Elsewhere,
}

/// The place the line names for a window: the postcode the engine reads in the window itself ("17, 26121 Oldenburg")
/// or, failing that, in the longest window that starts where this one does ("20 rue" of "20 rue du Vieux Port 13002
/// Marseille" is in 13002), and the name of a commune of the sheet written right after the window, right after that
/// postcode, or at the head of a comma-separated part of the line after the window ("…, 111 - Vila Moco, Petrolina").
#[derive(Debug, Default, PartialEq)]
struct Place {
    postcode: Option<u32>,
    city: Option<String>,
    /// the town is written in address form, after a comma or after the postcode: the place of the line even for a
    /// window that named a place of its own ("…, 56, Salamanca, 28006 Madrid")
    formal: bool,
}

const PLACE_STOP: [&str; 6] = ["le", "la", "les", "de", "du", "des"];

fn place_after(idx: &Index, text: &str, toks: &[Token], (i, j): (usize, usize)) -> Place {
    let on_line =
        |p: usize| p < toks.len() && toks[p].line == toks[i].line && toks[p].kind != Kind::Phone;
    // the place follows the window, however long the window is
    let mut last = j;
    while last + 1 < j + MAX_WORDS && on_line(last + 1) {
        last += 1;
    }
    let mut place = Place::default();
    let comma_before =
        |p: usize| p > 0 && p < toks.len() && text[toks[p - 1].end..toks[p].start].contains(',');
    // where a town may be written: (position, in address form)
    let mut city_at = vec![(j + 1, comma_before(j + 1))];
    let value = |p: usize| text[toks[p].start..toks[p].end].parse::<u32>().ok();
    let code =
        |p: usize| toks[p].kind == Kind::Number && text[toks[p].start..toks[p].end].len() >= 4;
    // a code the window holds after its first word, read by the engine in the window: the answer must agree with it
    let inside = (i + 1..=j).any(code).then(|| {
        let read = verdict::asked(idx, &text[toks[i].start..toks[j].end]).postcode;
        (i + 1..=j)
            .find(|&p| code(p) && read.is_some() && value(p) == read)
            .zip(read)
    });
    if let Some(Some((p, pc))) = inside {
        place.postcode = Some(pc);
        city_at.push((p + 1, true));
    } else if (j + 1..=last).any(code) {
        // a postcode has four digits or more: no query for a line of short numbers after the window
        let read = verdict::asked(idx, &text[toks[i].start..toks[last].end]).postcode;
        // the code the engine read is the first number after the window as written: not the number of a next
        // address
        let at = (j + 1..=last)
            .find(|&p| toks[p].kind == Kind::Number)
            .filter(|&p| read.is_some() && value(p) == read);
        if let (Some(pc), Some(p)) = (read, at) {
            place.postcode = Some(pc);
            city_at.push((p + 1, true));
        }
    }
    // a number and a street-type word start the next address of the line ("…, Ville, Cite 14 rue Haute")
    let next_address = |p: usize| {
        on_line(p)
            && on_line(p + 1)
            && toks[p].kind == Kind::Number
            && idx.is_street_type(&normalize(&text[toks[p + 1].start..toks[p + 1].end]))
    };
    let after = (j + 1..=last)
        .find(|&p| next_address(p))
        .unwrap_or(last + 1);
    city_at.extend(
        (j + 2..after)
            .filter(|&p| comma_before(p))
            .map(|p| (p, true)),
    );
    // the town the window wrote itself as its last part: the engine may have dropped it ("12 - Ouro Preto, Olinda"
    // answered in the town Ouro Preto); it counts only when nothing after the window names a town
    city_at.extend((i + 1..=j).filter(|&p| comma_before(p)).map(|p| (p, true)));
    city_at.retain(|&(p, _)| p < after);
    city_at.sort_by_key(|&(p, _)| (p > j, p));
    city_at.dedup_by_key(|c| c.0);
    let town = |(p, formal): (usize, bool)| {
        (1..=3).rev().find_map(|w| {
            let q = p + w - 1;
            if !(p..=q).all(|r| r <= last && on_line(r) && toks[r].kind == Kind::Word) {
                return None;
            }
            // the town written right before the next address is that address's town ("Cite 14 rue Haute")
            if next_address(q + 1) {
                return None;
            }
            let name = normalize(&text[toks[p].start..toks[q].end]);
            let words: Vec<&str> = name.split_whitespace().collect();
            // the town before its district: "Stuttgart" of "Stuttgart-West", longest name first
            (1..=words.len()).rev().find_map(|k| {
                let name = words[..k].join(" ");
                // a short or particle word names no place: "le matin", "de nouveau"
                let letters = name.chars().filter(|c| c.is_alphabetic()).count();
                (letters >= 3
                    && !PLACE_STOP.contains(&name.as_str())
                    && !country_word(idx, &name)
                    && idx.names_commune(&name))
                .then_some((name, formal))
            })
        })
    };
    // the town a line writes last is its town: "…, 56, Salamanca, 28006 Madrid" is in Madrid
    if let Some((city, formal)) = city_at.into_iter().rev().find_map(town) {
        place.city = Some(city);
        place.formal = formal;
    }
    place
}

/// Does the answer's postcode agree with the one the line names (`Some(true)`), differ from it (`Some(false)`),
/// or is there nothing to compare? Codes agree when one starts the other ("75" of "75002").
fn postcode_agrees(place: &Place, h: &Hit) -> Option<bool> {
    let code: String = h.postcode.chars().filter(char::is_ascii_digit).collect();
    match (place.postcode, code.parse::<u32>().ok()) {
        (Some(a), Some(b)) => {
            let (a, b) = (a.to_string(), b.to_string());
            Some(a.starts_with(&b) || b.starts_with(&a))
        }
        _ => None,
    }
}

/// The codes of the line and of the answer begin with the same two digits: one area (a département, a province, a
/// German region, a US state group), where a sheet may keep another code for the same street ("110 Schloss Ln Moraine
/// OH 45439": 45418). "C. San Fernando, 55, 03001 Alicante" is not "Calle C" of San Fernando (11100).
fn same_area(place: &Place, h: &Hit) -> bool {
    let code: String = h.postcode.chars().filter(char::is_ascii_digit).collect();
    place.postcode.is_some_and(|pc| {
        let line = format!("{pc:0w$}", w = code.len());
        code.len() >= 2 && line.len() >= 2 && line[..2] == code[..2]
    })
}

/// Countries of the published sheets as people end an address with them; a sheet's own `countries_tail` adds its own.
const COUNTRY_NAMES: [&str; 20] = [
    "australia",
    "brasil",
    "brazil",
    "canada",
    "deutschland",
    "espana",
    "france",
    "germany",
    "italia",
    "italy",
    "mexico",
    "monaco",
    "nederland",
    "netherlands",
    "serbia",
    "spain",
    "srbija",
    "united states",
    "usa",
    "uzbekistan",
];

/// A country at the end of an address ("…, Brasil", "…, USA"): no town, even where a commune's name begins with
/// it ("Brasil Novo").
fn country_word(idx: &Index, name: &str) -> bool {
    let rules = idx.rule_set();
    COUNTRY_NAMES.contains(&name)
        || rules.countries_tail.contains(name)
        || rules
            .city_tail
            .get("junk_tail")
            .is_some_and(|junk| junk.split_whitespace().any(|w| w == name))
}

/// The answer is farther from every commune the line names than the engine keeps an answer from a named place.
fn city_contradicts(idx: &Index, place: &Place, h: &Hit) -> bool {
    place.city.as_deref().is_some_and(|name| {
        // the answer's own commune bears the name: a municipality wider than the radius ("Santarém") is still it
        let own = normalize(&h.commune);
        let own_town = own == name || own.starts_with(&format!("{name} "));
        !(own_town || idx.near_commune_named(name, h.lat, h.lon))
    })
}

/// The street of the answer is one street-type word and nothing else ("Rue", "Calle"): what a window cut to
/// "20 rue" finds anywhere. "Court Street" names a street.
fn type_word_street(idx: &Index, h: &Hit) -> bool {
    let name = normalize(&h.street);
    let mut words = name.split_whitespace();
    matches!((words.next(), words.next()), (Some(w), None) if idx.is_street_type(w))
}

/// The window's house number ends its part of the line ("Quorvalix, 10, Arganzuela, 28045 Madrid", "R. 19 Sul, 8 -
/// Águas Claras", "Johannisstraße 17, 26121 Oldenburg"): the number belongs to the street written before it, and an
/// answer whose street shares no word with the words before the number read the district, the town or the code after
/// it as a street. "10, rue de Rivoli" — a street-type word after the comma — starts the street there.
fn street_from_another_part(
    idx: &Index,
    text: &str,
    toks: &[Token],
    (i, j): (usize, usize),
    h: &Hit,
) -> bool {
    let Some(n) = (i..=j).find(|&p| toks[p].kind == Kind::Number) else {
        return false;
    };
    let after = n + 1;
    if after >= toks.len() || toks[after].line != toks[n].line {
        return false;
    }
    let gap = &text[toks[n].end..toks[after].start];
    if !(gap.contains(',') || gap.contains('-')) {
        return false;
    }
    let word = |p: usize| normalize(&text[toks[p].start..toks[p].end]);
    if toks[after].kind == Kind::Word && idx.is_street_type(&word(after)) {
        return false;
    }
    let stem = |w: &str| w.chars().take(3).collect::<String>();
    let name = normalize(&h.street);
    let street: Vec<String> = name.split_whitespace().map(stem).collect();
    let compact: String = name.chars().filter(|c| !c.is_whitespace()).collect();
    let before: Vec<String> = (i..n)
        .flat_map(|p| {
            word(p)
                .split_whitespace()
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|w| !PLACE_STOP.contains(&w.as_str()))
        .collect();
    // the name words before the number; a street named by its type word alone ("Boulevard 4") has only that word
    let named: Vec<&String> = before.iter().filter(|w| !idx.is_street_type(w)).collect();
    let named = if named.is_empty() {
        before.iter().collect()
    } else {
        named
    };
    !named.iter().any(|w| {
        let letters = w.chars().filter(|c| c.is_alphabetic()).count();
        // a block of Mannheim ("F2 1") is written as one word, kept by the sheet as "F 2"
        (w.chars().any(|c| c.is_ascii_digit()) && w.len() >= 2 && compact.contains(w.as_str()))
            || (letters >= 3 && street.contains(&stem(w)))
    })
}

/// A window's answer, judged against the window and the line around it. The postcode of the line (after the
/// window) is checked unless the engine matched it or the window named both the street and the commune exactly;
/// the commune after the window is checked unless the window placed the answer itself (`commune_*`/`pc_exact`)
/// or the postcode already agrees.
fn judge(idx: &Index, text: &str, toks: &[Token], (i, j): (usize, usize), h: &Hit) -> Judged {
    let window = &text[toks[i].start..toks[j].end];
    let level = accept(idx, window, h);
    let placed = h
        .flags
        .iter()
        .any(|f| f.starts_with("commune_") || *f == "pc_exact" || *f == "place_exact");
    // "20 rue": a street called by its type word is any street; only the window's own place may confirm it
    if !placed && type_word_street(idx, h) {
        return Judged::Elsewhere;
    }
    if street_from_another_part(idx, text, toks, (i, j), h) {
        return Judged::Elsewhere;
    }
    let place = place_after(idx, text, toks, (i, j));
    let postcode = postcode_agrees(&place, h);
    // the postcode of the line outranks a commune the window seemed to name on a street it only guessed ("C. Pedro
    // de Toledo, 4, 29015 Málaga" is not "Calle Pedro Alcocer" of Toledo); the exact street of the commune the
    // window names stands against the code ("110 Schloss Ln Moraine OH 45439": the sheet keeps 45418), and so
    // does the engine's own match of that code
    // a district written before the code ("Centro, 28013 Madrid") is no town to check once the code agrees; a town
    // the line writes in address form after the window judges even a window that named a place of its own: the
    // district "Salamanca" of "…, 56, Salamanca, 28006 Madrid" is not the town Salamanca
    let city_off =
        postcode != Some(true) && (!placed || place.formal) && city_contradicts(idx, &place, h);
    // the code of the line may be off when the town is written: "808 S Hobart Blvd, Los Angeles, CA 60603"
    let town_agrees = place.city.is_some() && !city_contradicts(idx, &place, h);
    let named =
        placed && h.flags.contains(&"street_exact") && (same_area(&place, h) || town_agrees);
    if postcode == Some(false) && !named && !h.flags.contains(&"pc_exact") {
        return Judged::Elsewhere;
    }
    if city_off {
        return Judged::Elsewhere;
    }
    level.map_or(Judged::Missing, Judged::Found)
}

/// A window's answer is an address of the text: a house whose number is the asked one, or a street the query
/// named; never a weak answer (the engine's own "check before use") and never a POI.
fn accept(idx: &Index, window: &str, h: &Hit) -> Option<&'static str> {
    if h.flags.contains(&"poi_layer") || crate::query::hit_is_weak(h) {
        return None;
    }
    let v = verdict::field_verdicts(verdict::reading(idx, window), h);
    if h.precision == "house" && v.number == "confirmed" && v.street != "unresolved" {
        Some("house")
    } else if matches!(v.street, "confirmed" | "corrected")
        && matches!(h.precision, "street" | "interp" | "near" | "house")
    {
        Some("street")
    } else {
        None
    }
}

/// Of overlapping candidates keep a house before a street, a street found as written before one found with
/// a fix, a window the engine read whole before one it
/// trimmed, then the longest.
fn pick(mut cands: Vec<Candidate>, by_lead: bool) -> Vec<Candidate> {
    cands.sort_by(|a, b| {
        b.house
            .cmp(&a.house)
            .then(b.exact.cmp(&a.exact))
            .then(if by_lead {
                b.lead.cmp(&a.lead)
            } else {
                std::cmp::Ordering::Equal
            })
            .then(b.clean.cmp(&a.clean))
            .then(b.words.cmp(&a.words))
            .then(a.start.cmp(&b.start))
    });
    let mut kept: Vec<Candidate> = Vec::new();
    for c in cands {
        if kept.iter().all(|k| c.end <= k.start || k.end <= c.start) {
            kept.push(c);
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<(String, bool)> {
        tokens(text)
            .iter()
            .map(|t| (text[t.start..t.end].to_string(), t.kind == Kind::Phone))
            .collect()
    }

    #[test]
    fn a_phone_is_never_a_number_anchor() {
        let k = kinds("Call +381 11 1234567 or 011 222-33-44, Rue A 3");
        assert!(k
            .iter()
            .filter(|(w, _)| ["381", "11", "1234567", "011", "222-33-44"].contains(&w.as_str()))
            .all(|(_, p)| *p));
        assert!(
            k.iter().any(|(w, p)| w == "3" && !p),
            "a lone house number stays an anchor: {k:?}"
        );
    }

    #[test]
    fn house_numbers_take_a_letter_or_a_slash_tail() {
        assert!(is_house_number("12"));
        assert!(is_house_number("12a"));
        assert!(is_house_number("12/3"));
        assert!(!is_house_number("123456"));
        assert!(!is_house_number("12abc"));
    }

    #[test]
    fn offsets_are_bytes_of_the_text_as_given() {
        let text = "Ulica Đure 5";
        let t = tokens(text);
        assert_eq!(&text[t[1].start..t[1].end], "Đure");
        assert_eq!(&text[t[2].start..t[2].end], "5");
    }
}
