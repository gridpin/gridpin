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
fn split_glued_postcode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    let mut digit_run = 0usize;
    for c in s.chars() {
        if c.is_ascii_digit() {
            digit_run += 1;
        } else {
            if digit_run >= 4 && c.is_alphabetic() && !out.ends_with(' ') {
                out.push(' ');
            }
            digit_run = 0;
        }
        out.push(c);
    }
    out
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
