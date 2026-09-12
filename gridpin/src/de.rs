//! Germany-only query variants.
//!
//! German orthography is not a lossy global normalization rule: `ä` and `ae`
//! are equivalent in German input, but collapsing every `ae/oe/ue` in every
//! country would corrupt real French, Italian and Dutch names.  These helpers
//! are therefore called only by an index whose embedded metadata says
//! `country=de`.  The ordinary query is always tried first; these are bounded
//! alternatives, never replacements for a successful exact spelling.

#[cfg(test)]
use std::cell::RefCell;
use std::collections::HashSet;

use crate::norm::normalize;

#[cfg(test)]
thread_local! {
    static STREET_RUNTIME_TRACE: RefCell<Option<Vec<(String, String)>>> = const { RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn begin_street_runtime_trace() {
    STREET_RUNTIME_TRACE.with(|trace| *trace.borrow_mut() = Some(Vec::new()));
}

#[cfg(test)]
pub(crate) fn take_street_runtime_trace() -> Vec<(String, String)> {
    STREET_RUNTIME_TRACE.with(|trace| trace.borrow_mut().take().unwrap_or_default())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Effect {
    Orthography,
    CityAlias,
    OfficialCommuneAlias,
    Abbreviation,
    AdminTail,
    RecipientPrefix,
    SubaddressTail,
    ParentheticalSubaddress,
    AddressField,
    LocalityFirst,
    MissingCommaPostcodeBoundary,
    Country,
    HouseRange,
    HouseSlash,
    PostcodePrefix,
    PostcodeZero,
}

#[derive(Clone, Debug)]
pub(crate) struct QueryVariant {
    pub query: String,
    pub effects: Vec<Effect>,
    /// A locality-alias retry is admissible only when its canonical target is a
    /// commune in this exact sheet.  This prevents an English word or official
    /// long form from being rewritten into an unrelated sheet locality.
    pub required_commune: Option<String>,
}

fn push_unique(out: &mut Vec<QueryVariant>, seen: &mut HashSet<String>, variant: QueryVariant) {
    if seen.insert(variant.query.clone()) {
        out.push(variant);
    }
}

fn umlauts_to_digraphs(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 8);
    for ch in raw.chars() {
        match ch {
            'ä' => out.push_str("ae"),
            'ö' => out.push_str("oe"),
            'ü' => out.push_str("ue"),
            'Ä' => out.push_str("Ae"),
            'Ö' => out.push_str("Oe"),
            'Ü' => out.push_str("Ue"),
            'ß' | 'ẞ' => out.push_str("ss"),
            other => out.push(other),
        }
    }
    out
}

fn ascii_digraphs_to_german(normalized: &str) -> String {
    normalized
        .replace("ae", "a")
        .replace("oe", "o")
        .replace("ue", "u")
        .replace("ss", "ß")
}

/// Preserve the second half of a German house-number range for the suffix
/// dictionary.  The generic normalizer deliberately maps `15-17` to `15`; a
/// DE source such as Overture stores that row as numero=15, rep=17, so the
/// German retry needs `15 17`.  Slashes already become spaces in `normalize`.
fn range_separators_to_spaces(raw: &str) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::with_capacity(raw.len());
    for (i, ch) in chars.iter().copied().enumerate() {
        let numeric_range = matches!(ch, '-' | '–' | '—')
            && i > 0
            && i + 1 < chars.len()
            && chars[i - 1].is_ascii_digit()
            && chars[i + 1].is_ascii_digit();
        out.push(if numeric_range { ' ' } else { ch });
    }
    out
}

/// Truncate the right endpoint of a compact German house-number range when at
/// least one endpoint carries a letter suffix.  The generic normalizer handles
/// pure numeric `15-17`, but punctuation folding turns `3b-3c` into two house
/// tokens.  Keep this rule DE-local and deliberately narrow: no surrounding
/// spaces, one optional ASCII letter per endpoint, and one to four digits.
fn compact_letter_house_range_variants(raw: &str) -> Option<(String, String)> {
    let bytes = raw.as_bytes();
    for (separator, symbol) in raw
        .char_indices()
        .filter(|(_, symbol)| matches!(*symbol, '-' | '–' | '—'))
    {
        let mut left_start = separator;
        let left_has_letter = left_start > 0 && bytes[left_start - 1].is_ascii_alphabetic();
        if left_has_letter {
            left_start -= 1;
        }
        let left_digit_end = left_start;
        while left_start > 0 && bytes[left_start - 1].is_ascii_digit() {
            left_start -= 1;
        }
        let left_digits = left_digit_end - left_start;
        if !(1..=4).contains(&left_digits)
            || raw[..left_start]
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric)
        {
            continue;
        }

        let mut right_end = separator + symbol.len_utf8();
        let right_digit_start = right_end;
        while right_end < bytes.len() && bytes[right_end].is_ascii_digit() {
            right_end += 1;
        }
        let right_digits = right_end - right_digit_start;
        if !(1..=4).contains(&right_digits) {
            continue;
        }
        let right_has_letter = right_end < bytes.len() && bytes[right_end].is_ascii_alphabetic();
        if right_has_letter {
            right_end += 1;
        }
        if !(left_has_letter || right_has_letter)
            || raw[right_end..]
                .chars()
                .next()
                .is_some_and(char::is_alphanumeric)
        {
            continue;
        }

        return Some((
            format!("{}{}", &raw[..separator], &raw[right_end..]),
            format!(
                "{} {}",
                &raw[..separator],
                &raw[separator + symbol.len_utf8()..]
            ),
        ));
    }
    None
}

/// Keep a first-endpoint fallback for a compact pure-numeric range.  The caller
/// uses this only inside the postcode-bound official-commune alias path, and
/// appends it after the ordinary/full-range candidates.  An exact source range
/// therefore keeps precedence; the first endpoint is tried only when the sheet
/// represents the interval by that endpoint alone.
fn compact_numeric_house_range_first_endpoint(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    for (separator, symbol) in raw
        .char_indices()
        .filter(|(_, symbol)| matches!(*symbol, '-' | '–' | '—'))
    {
        let mut left_start = separator;
        while left_start > 0 && bytes[left_start - 1].is_ascii_digit() {
            left_start -= 1;
        }
        let left_digits = separator - left_start;
        if !(1..=4).contains(&left_digits)
            || raw[..left_start]
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric)
        {
            continue;
        }

        let right_start = separator + symbol.len_utf8();
        let mut right_end = right_start;
        while right_end < bytes.len() && bytes[right_end].is_ascii_digit() {
            right_end += 1;
        }
        let right_digits = right_end - right_start;
        if !(1..=4).contains(&right_digits)
            || raw[right_end..]
                .chars()
                .next()
                .is_some_and(char::is_alphanumeric)
        {
            continue;
        }
        return Some(format!("{}{}", &raw[..separator], &raw[right_end..]));
    }
    None
}

/// German source data contains mixed fractions such as `17 1/2`.  The common
/// user shorthand `17/2` normalizes to suffix `2`, while the prepared source
/// suffix is `12`.  Keep the ordinary slash interpretation and offer this
/// second, marked retry.  Only `/2` is admitted: that is the live shorthand
/// proven by the frozen corpus, while `7/8`, `36/38`, and similar forms are
/// ordinary multi-number addresses and remain on the ordinary path.
fn slash_to_mixed_fraction(raw: &str) -> Option<String> {
    let normalized_raw = normalize(raw);
    if contains_token_phrase(&normalized_raw, "bis")
        || contains_token_phrase(&normalized_raw, "und")
    {
        return None;
    }
    let mut slashes = raw.match_indices('/');
    let (slash, _) = slashes.next()?;
    if slashes.next().is_some() {
        return None;
    }
    let bytes = raw.as_bytes();
    if slash == 0
        || slash + 1 >= bytes.len()
        || !bytes[slash - 1].is_ascii_digit()
        || bytes[slash + 1] != b'2'
        || bytes
            .get(slash + 2)
            .is_some_and(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let left_start = bytes[..slash]
        .iter()
        .rposition(|byte| !byte.is_ascii_digit())
        .map_or(0, |position| position + 1);
    // `17/2` is shorthand for a mixed fraction in the accepted source, but
    // `17 1/2` is already explicit and must not gain a second numerator.
    if &raw[left_start..slash] == "1"
        && raw[..left_start]
            .trim_end()
            .as_bytes()
            .last()
            .is_some_and(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    Some(normalize(&format!(
        "{} 1 {}",
        &raw[..slash],
        &raw[slash + 1..]
    )))
}

fn expand_range_words(normalized: &str) -> Option<String> {
    let tokens: Vec<&str> = normalized.split_whitespace().collect();
    let mut out = Vec::with_capacity(tokens.len());
    let mut changed = false;
    let mut i = 0;
    while i < tokens.len() {
        if i + 2 < tokens.len()
            && tokens[i].bytes().all(|b| b.is_ascii_digit())
            && matches!(tokens[i + 1], "bis" | "und")
            && tokens[i + 2].bytes().all(|b| b.is_ascii_digit())
        {
            out.push(tokens[i]);
            out.push(tokens[i + 2]);
            changed = true;
            i += 3;
            continue;
        }
        out.push(tokens[i]);
        i += 1;
    }
    changed.then(|| out.join(" "))
}

fn strip_postcode_country_prefix(normalized: &str) -> Option<String> {
    let tokens: Vec<&str> = normalized.split_whitespace().collect();
    let mut changed = false;
    let mut out = Vec::with_capacity(tokens.len());
    let mut i = 0;
    while i < tokens.len() {
        let token = tokens[i];
        let bytes = token.as_bytes();
        if bytes.len() == 6 && bytes[0] == b'd' && bytes[1..].iter().all(u8::is_ascii_digit) {
            out.push(&token[1..]);
            changed = true;
            i += 1;
            continue;
        }
        if matches!(token, "d" | "de")
            && tokens.get(i + 1).is_some_and(|postcode| {
                postcode.len() == 5 && postcode.bytes().all(|byte| byte.is_ascii_digit())
            })
        {
            changed = true;
            i += 1;
            continue;
        }
        out.push(token);
        i += 1;
    }
    changed.then(|| out.join(" "))
}

/// Remove a typed recipient segment only when it occupies the complete raw
/// comma-delimited prefix.  Keeping the comma boundary in the predicate is
/// important: `An die Poststelle 7` may itself be source text, while
/// `An die Poststelle, Hauptstrasse 7` is ordinary delivery noise.
fn strip_recipient_prefix_raw(raw: &str) -> Option<String> {
    let (prefix, address) = raw.split_once(',')?;
    let prefix = normalize(prefix);
    if !matches!(
        prefix.as_str(),
        "z hd empfang" | "fur den empfang" | "an die poststelle"
    ) || !address.chars().any(|character| character.is_ascii_digit())
    {
        return None;
    }
    let address = address.trim();
    (!address.is_empty()).then(|| address.to_string())
}

/// Remove only the exact delivery-detail segments observed after the final
/// comma.  This deliberately does not use a global stop-word list: words such
/// as `Hinterhaus` and `Erdgeschoss` remain legal street/locality text unless
/// the whole trailing segment has the proven subaddress shape.
fn strip_subaddress_tail_raw(raw: &str) -> Option<String> {
    let (address, tail) = raw.rsplit_once(',')?;
    let tail = normalize(tail);
    if !matches!(
        tail.as_str(),
        "aufgang b 3 og" | "hinterhaus 1 og" | "2 og" | "erdgeschoss"
    ) || !address.chars().any(|character| character.is_ascii_digit())
    {
        return None;
    }
    let address = address.trim();
    (!address.is_empty()).then(|| address.to_string())
}

/// Remove a terminal phone number only when it is a complete comma-delimited
/// delivery tail.  Requiring an explicit `Tel`/`Telefon` marker, at least five
/// digits, and no alphabetic payload keeps real locality/street text intact.
fn strip_phone_tail_raw(raw: &str) -> Option<String> {
    let (address, tail) = raw.rsplit_once(',')?;
    let tail = normalize(tail);
    let mut tokens = tail.split_whitespace();
    if !matches!(tokens.next(), Some("tel" | "telefon")) {
        return None;
    }
    let payload = tokens.collect::<Vec<_>>().join(" ");
    if payload.chars().filter(char::is_ascii_digit).count() < 5
        || payload.chars().any(char::is_alphabetic)
        || !address.chars().any(|character| character.is_ascii_digit())
    {
        return None;
    }
    let address = address.trim();
    (!address.is_empty()).then(|| address.to_string())
}

fn strip_delivery_tail_raw(raw: &str) -> Option<String> {
    strip_subaddress_tail_raw(raw).or_else(|| strip_phone_tail_raw(raw))
}

fn bounded_house_endpoint(value: &str) -> bool {
    let mut chars = value.chars();
    let mut digits = 0;
    while chars.clone().next().is_some_and(|ch| ch.is_ascii_digit()) {
        chars.next();
        digits += 1;
    }
    if !(1..=4).contains(&digits) {
        return false;
    }
    match (chars.next(), chars.next()) {
        (None, None) => true,
        (Some(suffix), None) => suffix.is_alphabetic(),
        _ => false,
    }
}

fn bounded_house_expression(value: &str) -> bool {
    let value = value.trim();
    if bounded_house_endpoint(value) {
        return true;
    }
    let mut separator = None;
    for (index, character) in value.char_indices() {
        if matches!(character, '-' | '–' | '—' | '/') {
            if separator.is_some() {
                return false;
            }
            separator = Some((index, character.len_utf8()));
        }
    }
    separator.is_some_and(|(index, width)| {
        bounded_house_endpoint(&value[..index]) && bounded_house_endpoint(&value[index + width..])
    })
}

fn ends_with_bounded_house(field: &str) -> bool {
    let mut tokens = field.split_whitespace().rev();
    let Some(last) = tokens.next() else {
        return false;
    };
    if bounded_house_expression(last) {
        return true;
    }
    last.chars().count() == 1
        && last.chars().all(char::is_alphabetic)
        && tokens.next().is_some_and(bounded_house_endpoint)
}

fn is_address_field(field: &str) -> bool {
    field.chars().any(char::is_alphabetic) && ends_with_bounded_house(field)
}

fn bounded_parenthetical_house_endpoint(value: &str) -> bool {
    let mut chars = value.chars();
    let mut digits = 0;
    while chars.clone().next().is_some_and(|ch| ch.is_ascii_digit()) {
        chars.next();
        digits += 1;
    }
    if !(1..=5).contains(&digits) {
        return false;
    }
    match (chars.next(), chars.next()) {
        (None, None) => true,
        (Some(suffix), None) => suffix.is_ascii_alphabetic(),
        _ => false,
    }
}

fn bounded_parenthetical_house_expression(value: &str) -> bool {
    if bounded_parenthetical_house_endpoint(value) {
        return true;
    }
    let mut separator = None;
    for (index, character) in value.char_indices() {
        if matches!(character, '-' | '–' | '—' | '/') {
            if separator.is_some() {
                return false;
            }
            separator = Some((index, character.len_utf8()));
        }
    }
    separator.is_some_and(|(index, width)| {
        bounded_parenthetical_house_endpoint(&value[..index])
            && bounded_parenthetical_house_endpoint(&value[index + width..])
    })
}

/// Remove one typed building/subaddress label from the exact raw shape
/// `street house (label), postcode locality`.
///
/// Parentheses are common legal street/locality punctuation, so this is not a
/// generic bracket stripper.  The label must be the only balanced, non-nested
/// parenthetical segment, immediately follow one bounded house expression and
/// immediately precede the postcode comma.  The terminal locality is retained
/// separately so the runtime can require exact commune-core identity before a
/// previously empty result may be filled.
fn parenthetical_subaddress_raw(raw: &str) -> Option<(String, String)> {
    if raw
        .chars()
        .any(|character| matches!(character, '\n' | '\r' | '\t'))
        || raw.chars().filter(|character| *character == '(').count() != 1
        || raw.chars().filter(|character| *character == ')').count() != 1
    {
        return None;
    }

    let close = raw.rfind("),")?;
    let open = raw[..close].rfind('(')?;
    let before_open = &raw[..open];
    if !before_open
        .chars()
        .next_back()
        .is_some_and(char::is_whitespace)
    {
        return None;
    }
    let address = before_open.trim_end();
    let house = address.split_whitespace().next_back()?;
    let street = address.strip_suffix(house)?.trim_end();
    if address.is_empty()
        || address.trim_start() != address
        || address
            .chars()
            .any(|character| matches!(character, ',' | ';' | '|'))
        || street.is_empty()
        || !street.chars().any(char::is_alphabetic)
        || !bounded_parenthetical_house_expression(house)
    {
        return None;
    }
    let normalized_address = normalize(address);
    if normalized_address == "postfach"
        || normalized_address.starts_with("postfach ")
        || normalized_address == "po box"
        || normalized_address.starts_with("po box ")
    {
        return None;
    }

    let label = &raw[open + 1..close];
    let label_len = label.chars().count();
    if !(1..=80).contains(&label_len) || !label.chars().any(char::is_alphabetic) {
        return None;
    }

    let terminal = raw[close + 2..].trim_start();
    let boundary = terminal.find(char::is_whitespace)?;
    let postcode = &terminal[..boundary];
    let locality = terminal[boundary..].trim();
    if postcode.len() != 5
        || !postcode.bytes().all(|byte| byte.is_ascii_digit())
        || locality.is_empty()
        || !locality.chars().any(char::is_alphabetic)
        || locality
            .chars()
            .any(|character| character.is_ascii_digit() || matches!(character, ',' | ';' | '|'))
    {
        return None;
    }

    Some((
        format!("{address}, {postcode} {locality}"),
        normalize(locality),
    ))
}

fn strip_parenthetical_subaddress_raw(raw: &str) -> Option<String> {
    parenthetical_subaddress_raw(raw).map(|(query, _)| query)
}

pub(crate) fn parenthetical_subaddress_commune(raw: &str) -> Option<String> {
    parenthetical_subaddress_raw(raw).map(|(_, commune)| commune)
}

/// Select one address-bearing field from the exact pasted shape
/// `field, field, postcode locality`.
///
/// This is intentionally additive and narrower than generic POI stripping:
/// exactly one of the first two fields must end in a bounded house token, the
/// other field may not itself be an address or a numeric house continuation,
/// and the selected field may not be a generic building/subaddress label.  The
/// ordinary raw query remains first, so an already exact answer still owns a
/// tie.
fn select_three_field_address_raw(raw: &str) -> Option<String> {
    let fields: Vec<&str> = raw.split(',').map(str::trim).collect();
    if fields.len() != 3 || fields.iter().any(|field| field.is_empty()) {
        return None;
    }
    let (postcode, locality) = fields[2].split_once(' ')?;
    if postcode.len() != 5
        || !postcode.bytes().all(|byte| byte.is_ascii_digit())
        || locality.is_empty()
        || locality.starts_with(' ')
        || !locality.chars().any(char::is_alphabetic)
        || locality.chars().any(|character| character.is_ascii_digit())
    {
        return None;
    }

    let shapes = [is_address_field(fields[0]), is_address_field(fields[1])];
    let selected = match shapes {
        [true, false] => 0,
        [false, true] => 1,
        _ => return None,
    };
    let other = fields[1 - selected];
    if ends_with_bounded_house(other)
        || other
            .split_whitespace()
            .any(|token| token.len() == 5 && token.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return None;
    }
    let selected_norm = normalize(fields[selected]);
    if selected_norm
        .split_whitespace()
        .next()
        .is_some_and(|token| matches!(token, "gebaude" | "haus" | "tor" | "building" | "campus"))
    {
        return None;
    }
    Some(format!("{}, {}", fields[selected], fields[2]))
}

/// Restore one comma that was mechanically replaced by exactly three ASCII
/// spaces between `street house` and `postcode locality`.
///
/// The boundary is deliberately much narrower than generic whitespace
/// repair.  It rejects multiple candidate boundaries, ranges/lists, tabs,
/// comma-bearing input, non-terminal house tokens and any right-hand side
/// other than one five-digit postcode plus a non-numeric locality.  This shape
/// is produced by the frozen dirty-corpus `comma_removal_whitespace`
/// transform, while ordinary multi-field listings remain untouched.
fn restore_missing_postcode_comma_raw(raw: &str) -> Option<String> {
    if raw.contains(',')
        || raw
            .chars()
            .any(|character| matches!(character, '\t' | '\n' | '\r'))
    {
        return None;
    }

    let bytes = raw.as_bytes();
    let mut boundary = None;
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b' ' {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && bytes[index] == b' ' {
            index += 1;
        }
        match index - start {
            1 => {}
            3 if boundary.is_none() => boundary = Some(start),
            _ => return None,
        }
    }

    let boundary = boundary?;
    let left = &raw[..boundary];
    let right = &raw[boundary + 3..];
    if left.is_empty()
        || right.is_empty()
        || left.trim() != left
        || right.trim() != right
        || left.contains('/')
    {
        return None;
    }

    let (street, house) = left.rsplit_once(' ')?;
    if street.is_empty()
        || !street.chars().any(char::is_alphabetic)
        || !(1..=4).contains(&house.len())
        || !house.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let normalized_left = normalize(left);
    let left_tokens: Vec<&str> = normalized_left.split_whitespace().collect();
    let raw_left_tokens: Vec<&str> = left.split_whitespace().collect();
    if left_tokens
        .iter()
        .any(|token| matches!(*token, "bis" | "und"))
        || (raw_left_tokens.len() >= 3
            && raw_left_tokens[raw_left_tokens.len() - 3]
                .bytes()
                .all(|byte| byte.is_ascii_digit())
            && matches!(raw_left_tokens[raw_left_tokens.len() - 2], "-" | "–" | "—"))
    {
        return None;
    }

    let (postcode, locality) = right.split_once(' ')?;
    if postcode.len() != 5
        || !postcode.bytes().all(|byte| byte.is_ascii_digit())
        || locality.is_empty()
        || locality.starts_with(' ')
        || !locality.chars().any(char::is_alphabetic)
        || locality.chars().any(|character| {
            character.is_ascii_digit() || matches!(character, ',' | ';' | '|' | '\n' | '\r')
        })
    {
        return None;
    }

    Some(format!("{left}, {right}"))
}

/// Recover the common pasted order `locality, street house, postcode`.
/// Exactly three comma-delimited fields and an isolated five-digit terminal
/// postcode keep the transform bounded and prevent POI-prefix queries such as
/// `Museum, Platz 1, 10115 Berlin` from being reordered.
fn reorder_locality_first_raw(raw: &str) -> Option<String> {
    let fields: Vec<&str> = raw.split(',').map(str::trim).collect();
    if fields.len() != 3
        || fields.iter().any(|field| field.is_empty())
        || normalize(fields[2]).len() != 5
        || !normalize(fields[2])
            .bytes()
            .all(|byte| byte.is_ascii_digit())
        || !normalize(fields[1])
            .split_whitespace()
            .next_back()
            .is_some_and(|token| token.starts_with(|character: char| character.is_ascii_digit()))
        || fields[0]
            .chars()
            .any(|character| character.is_ascii_digit())
    {
        return None;
    }
    Some(format!("{}, {} {}", fields[1], fields[2], fields[0]))
}

/// Preserve raw comma boundaries for the small structural cleanup chain.  The
/// normalized forms still feed ordinary retries, while these raw forms let the
/// parser retain its strongest address-field evidence.
fn raw_structural_cleanup_seeds(
    raw: &str,
    initial_effects: Vec<Effect>,
) -> Vec<(String, Vec<Effect>)> {
    let mut seeds = Vec::new();
    let mut prepared_raw = raw.to_string();
    let mut prepared_effects = initial_effects;
    for (effect, transform) in [
        (
            Effect::MissingCommaPostcodeBoundary,
            restore_missing_postcode_comma_raw as fn(&str) -> Option<String>,
        ),
        (
            Effect::RecipientPrefix,
            strip_recipient_prefix_raw as fn(&str) -> Option<String>,
        ),
        (
            Effect::ParentheticalSubaddress,
            strip_parenthetical_subaddress_raw,
        ),
        (Effect::SubaddressTail, strip_delivery_tail_raw),
        (Effect::AddressField, select_three_field_address_raw),
        (Effect::LocalityFirst, reorder_locality_first_raw),
    ] {
        if let Some(value) = transform(&prepared_raw) {
            prepared_raw = value;
            prepared_effects.push(effect);
            seeds.push((prepared_raw.clone(), prepared_effects.clone()));
        }
    }
    seeds
}

/// `Binz OT Prora` and `Vierlinden Ortsteil Friedersdorf` name a municipality
/// followed by a subordinate locality.  The public address sheet indexes the
/// municipality.  Keep it and remove the typed tail.  This is intentionally not
/// the generic `region_markers` behavior, which removes the word *before* a
/// marker and would turn `Binz OT Prora` into `Prora`.
fn strip_admin_tail(normalized: &str) -> Option<String> {
    let rules = crate::rules::rules();
    let tokens: Vec<&str> = normalized.split_whitespace().collect();
    let marker = tokens
        .iter()
        .enumerate()
        .find_map(|(i, token)| (i > 0 && rules.de_admin_tail.contains(*token)).then_some(i))?;
    Some(tokens[..marker].join(" "))
}

fn strip_country_tokens(normalized: &str) -> Option<String> {
    let countries = &crate::rules::rules().de_countries;
    let tokens: Vec<&str> = normalized.split_whitespace().collect();
    let kept: Vec<&str> = tokens
        .iter()
        .copied()
        .filter(|token| !countries.contains(*token))
        .collect();
    (kept.len() != tokens.len()).then(|| kept.join(" "))
}

fn replace_token_phrase(normalized: &str, needle: &str, replacement: &str) -> Option<String> {
    let words: Vec<&str> = normalized.split_whitespace().collect();
    let key: Vec<&str> = needle.split_whitespace().collect();
    if key.is_empty() || key.len() > words.len() {
        return None;
    }
    let start = words
        .windows(key.len())
        .position(|window| window == key.as_slice())?;
    let mut out: Vec<String> = Vec::with_capacity(words.len() + 2);
    out.extend(words[..start].iter().map(|word| (*word).to_string()));
    out.extend(replacement.split_whitespace().map(str::to_string));
    out.extend(
        words[start + key.len()..]
            .iter()
            .map(|word| (*word).to_string()),
    );
    Some(out.join(" "))
}

fn replace_tail_phrase(normalized: &str, needle: &str, replacement: &str) -> Option<String> {
    if normalized == needle {
        return Some(replacement.to_string());
    }
    let prefix = normalized.strip_suffix(needle)?.strip_suffix(' ')?;
    Some(format!("{prefix} {replacement}"))
}

fn expand_de_abbreviation(normalized: &str) -> Option<String> {
    crate::rules::rules()
        .de_abbrev2
        .iter()
        .find_map(|(short, full)| replace_token_phrase(normalized, short, full))
        .or_else(|| expand_de_street_type_abbreviation(normalized))
}

/// Expand a short German street-type token only when it immediately precedes
/// a house token.  This context prevents ordinary one- and two-letter words
/// elsewhere in a name from being rewritten.
fn expand_de_street_type_abbreviation(normalized: &str) -> Option<String> {
    let mut tokens: Vec<&str> = normalized.split_whitespace().collect();
    let index = tokens.windows(2).position(|window| {
        matches!(window[0], "str" | "pl" | "al" | "uf" | "wg" | "g" | "ch")
            && window[1]
                .chars()
                .next()
                .is_some_and(|character| character.is_ascii_digit())
    })?;
    tokens[index] = match tokens[index] {
        "str" => "straße",
        "pl" => "platz",
        "al" => "allee",
        "uf" => "ufer",
        "wg" => "weg",
        "g" => "gasse",
        "ch" => "chaussee",
        _ => unreachable!("matched German street-type abbreviation"),
    };
    Some(tokens.join(" "))
}

/// Expand an attached `…pl.` only when the raw punctuation proves it is an
/// abbreviation and the following token is a house number.  Normalization
/// erases that final dot, so this deliberately operates on the raw candidate.
fn expand_attached_pl_abbreviation_raw(raw: &str) -> Option<String> {
    let mut tokens: Vec<String> = raw.split_whitespace().map(str::to_string).collect();
    let index = tokens.windows(2).position(|window| {
        let street = window[0].to_lowercase();
        let Some(prefix) = street.strip_suffix("pl.") else {
            return false;
        };
        prefix
            .chars()
            .filter(|character| character.is_alphabetic())
            .count()
            >= 3
            && window[1]
                .chars()
                .next()
                .is_some_and(|character| character.is_ascii_digit())
    })?;
    let street = &tokens[index];
    tokens[index] = format!("{}platz", &street[..street.len() - 3]);
    Some(tokens.join(" "))
}

fn expand_de_city_alias(normalized: &str) -> Option<(String, String)> {
    crate::rules::rules()
        .de_city_alias
        .iter()
        .find_map(|(alias, full)| {
            replace_tail_phrase(normalized, alias, full).map(|query| (query, full.to_string()))
        })
}

/// A few German municipalities have one bounded official/public alias in the
/// source or query. This is deliberately not a general locality rewrite: the
/// alias must be the complete tail immediately following a five-digit postcode.
/// Runtime acceptance adds independent exact street+house and uniqueness
/// postconditions.
fn expand_de_official_commune_alias(normalized: &str) -> Option<(String, String)> {
    const ALIASES: [(&str, &str); 4] = [
        ("bad homburg vor der hohe", "bad homburg v d hohe"),
        ("limburg an der lahn", "limburg a d lahn"),
        ("rotenburg an der fulda", "rotenburg a d fulda"),
        ("ludwigshafen rhein", "ludwigshafen am rhein"),
    ];

    ALIASES.iter().find_map(|(long, abbreviated)| {
        let prefix = normalized.strip_suffix(long)?.strip_suffix(' ')?;
        let postcode = prefix.split_whitespace().next_back()?;
        (postcode.len() == 5 && postcode.as_bytes().iter().all(u8::is_ascii_digit)).then(|| {
            (
                format!("{prefix} {abbreviated}"),
                (*abbreviated).to_string(),
            )
        })
    })
}

/// The Ludwigshafen alias is admitted only for the frozen two-field,
/// single-integer-house product surface. In particular `44/48` remains a
/// house-set query and cannot inherit this single-house promotion path.
fn de_ludwigshafen_official_alias_surface(raw: &str) -> bool {
    if raw.trim() != raw
        || raw.len() > 256
        || raw
            .chars()
            .any(|character| matches!(character, '\n' | '\r' | '\t' | ';' | '|'))
    {
        return false;
    }
    let mut fields = raw.split(',');
    let Some(address) = fields.next().map(str::trim) else {
        return false;
    };
    let Some(terminal) = fields.next().map(str::trim) else {
        return false;
    };
    if address.is_empty() || terminal.is_empty() || fields.next().is_some() {
        return false;
    }
    let Some(split) = address.rfind(char::is_whitespace) else {
        return false;
    };
    let street = address[..split].trim_end();
    let house = address[split..].trim();
    if street.is_empty()
        || !street.chars().any(char::is_alphabetic)
        || street.chars().any(|character| character.is_ascii_digit())
        || house.is_empty()
        || house.len() > 4
        || house.as_bytes().first() == Some(&b'0')
        || !house.bytes().all(|byte| byte.is_ascii_digit())
        || house.parse::<u32>().ok().is_none_or(|number| number == 0)
    {
        return false;
    }
    let mut terminal_tokens = terminal.split_whitespace();
    let Some(postcode) = terminal_tokens.next() else {
        return false;
    };
    postcode.len() == 5
        && postcode.bytes().all(|byte| byte.is_ascii_digit())
        && normalize(&terminal_tokens.collect::<Vec<_>>().join(" ")) == "ludwigshafen rhein"
}

fn pad_lost_postcode_zero(normalized: &str) -> Option<String> {
    let tokens: Vec<&str> = normalized.split_whitespace().collect();
    let pos = tokens.iter().enumerate().find_map(|(i, token)| {
        (i > 0
            && i + 1 < tokens.len()
            && token.len() == 4
            && token.as_bytes().iter().all(u8::is_ascii_digit)
            && tokens[i + 1].chars().any(char::is_alphabetic))
        .then_some(i)
    })?;
    let mut out: Vec<String> = tokens.iter().map(|s| (*s).to_string()).collect();
    out[pos] = format!("0{}", tokens[pos]);
    Some(out.join(" "))
}

fn derived_seeds(raw: &str) -> Vec<(String, Vec<Effect>)> {
    let mut seeds = Vec::new();
    let base = normalize(raw);
    let compact_letter_range = compact_letter_house_range_variants(raw);
    let compact_letter_range_effects = compact_letter_range
        .as_ref()
        .map_or_else(Vec::new, |_| vec![Effect::HouseRange]);
    let base_effects = compact_letter_range_effects.clone();
    seeds.push((base.clone(), base_effects.clone()));

    // Delivery noise and locality-first ordering are derived from raw comma
    // boundaries before normalization erases that evidence.  Apply them as a
    // short cumulative chain so realistic combinations remain bounded.
    let structural_raw = raw_structural_cleanup_seeds(raw, base_effects.clone());
    for (prepared_raw, prepared_effects) in &structural_raw {
        let normalized = normalize(prepared_raw);
        if normalized != base {
            seeds.push((normalized, prepared_effects.clone()));
        }
    }

    // Attached Platz abbreviations need the raw final dot as evidence.  Apply
    // them both to the original input and to bounded delivery-cleanup forms so
    // realistic combinations do not require an unsafe global suffix rewrite.
    let mut raw_candidates = vec![(raw.to_string(), base_effects.clone())];
    raw_candidates.extend(structural_raw);
    for (candidate, mut effects) in raw_candidates {
        if let Some(expanded) = expand_attached_pl_abbreviation_raw(&candidate) {
            effects.push(Effect::Abbreviation);
            let normalized = normalize(&expanded);
            seeds.push((normalized.clone(), effects.clone()));
            let range = normalize(&range_separators_to_spaces(&expanded));
            if range != normalized {
                if !effects.contains(&Effect::HouseRange) {
                    effects.push(Effect::HouseRange);
                }
                seeds.push((range, effects));
            }
        }
    }

    if let Some((truncated, expanded)) = compact_letter_range {
        let truncated = normalize(&truncated);
        if truncated != base {
            seeds.push((truncated, compact_letter_range_effects.clone()));
        }
        let expanded = normalize(&expanded);
        if expanded != base {
            seeds.push((expanded, compact_letter_range_effects));
        }
    }

    let digraph = normalize(&umlauts_to_digraphs(raw));
    if digraph != base {
        let mut effects = base_effects.clone();
        effects.push(Effect::Orthography);
        seeds.push((digraph, effects));
    }

    let range = normalize(&range_separators_to_spaces(raw));
    if range != base {
        let mut effects = base_effects.clone();
        if !effects.contains(&Effect::HouseRange) {
            effects.push(Effect::HouseRange);
        }
        seeds.push((range.clone(), effects));
    }
    if let Some(words) = expand_range_words(&range) {
        let mut effects = base_effects;
        if !effects.contains(&Effect::HouseRange) {
            effects.push(Effect::HouseRange);
        }
        seeds.push((words, effects));
    }
    if let Some(fraction) = slash_to_mixed_fraction(raw) {
        seeds.push((fraction, vec![Effect::HouseSlash]));
    }
    seeds
}

/// Whether the original raw DE input may enter the independent c2 postal-tail rule.
/// Inspect the unbounded structural seeds: `query_variants` is intentionally capped, so a
/// compositional input can push its range/slash variant past that output limit even though
/// the raw syntax must still reserve precedence for house-range handling.
pub(crate) fn postal_tail_eligible(raw: &str) -> bool {
    !derived_seeds(raw).iter().any(|(_, effects)| {
        effects
            .iter()
            .any(|effect| matches!(*effect, Effect::HouseRange | Effect::HouseSlash))
    })
}

fn contains_token_phrase(haystack: &str, needle: &str) -> bool {
    haystack == needle
        || haystack.starts_with(&format!("{needle} "))
        || haystack.ends_with(&format!(" {needle}"))
        || haystack.contains(&format!(" {needle} "))
}

/// An explicit Frankfurt qualifier is a hard constraint, never disposable
/// trailing noise.  Bare `Frankfurt` intentionally returns None: a unique street
/// or postcode may still identify either city without a global alias.
pub(crate) fn frankfurt_qualifier(raw: &str) -> Option<&'static str> {
    let normalized = normalize(raw);
    if contains_token_phrase(&normalized, "frankfurt oder") {
        Some("frankfurt oder")
    } else if contains_token_phrase(&normalized, "frankfurt am main")
        || contains_token_phrase(&normalized, "frankfurt main")
        || contains_token_phrase(&normalized, "frankfurt a m")
    {
        Some("frankfurt am main")
    } else {
        None
    }
}

/// Bounded, compositional alternatives for a DE query.  The raw spelling is
/// element zero so ties preserve existing behavior, except for a compact
/// letter-suffixed house range: punctuation folding can reinterpret `11-13c`
/// as the different exact house `11c`, so the marked first endpoint must own
/// that tie.
pub(crate) fn query_variants(raw: &str) -> Vec<QueryVariant> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    // Structural rewrites such as locality-first ordering may move a long-form
    // locality to the tail.  They must not manufacture the postcode-bound raw
    // syntax that authorizes this separate alias path.
    let official_commune_alias = expand_de_official_commune_alias(&normalize(raw));
    let official_commune_alias_allowed =
        official_commune_alias.as_ref().is_some_and(|(_, target)| {
            target != "ludwigshafen am rhein" || de_ludwigshafen_official_alias_surface(raw)
        });
    let mut seeds = derived_seeds(raw);
    if official_commune_alias_allowed {
        if let Some(first_endpoint) = compact_numeric_house_range_first_endpoint(raw) {
            let first_endpoint = normalize(&first_endpoint);
            if !seeds.iter().any(|(query, _)| query == &first_endpoint) {
                seeds.push((first_endpoint, vec![Effect::HouseRange]));
            }
        }
    }
    let compact_primary = compact_letter_house_range_variants(raw)
        .map(|(truncated, _)| normalize(&truncated))
        .and_then(|truncated| {
            seeds
                .iter()
                .find(|(seed, effects)| seed == &truncated && effects.contains(&Effect::HouseRange))
                .cloned()
        });
    let (first_query, first_effects) =
        compact_primary.unwrap_or_else(|| (raw.to_string(), Vec::new()));
    push_unique(
        &mut out,
        &mut seen,
        QueryVariant {
            query: first_query,
            effects: first_effects,
            required_commune: None,
        },
    );

    // Structural cleanup is most valuable before normalization destroys raw
    // comma boundaries.  Keep these bounded candidates ahead of the global
    // variant cap; the normalized equivalents are still generated below.
    let raw_effects = compact_letter_house_range_variants(raw)
        .map_or_else(Vec::new, |_| vec![Effect::HouseRange]);
    for (query, effects) in raw_structural_cleanup_seeds(raw, raw_effects) {
        push_unique(
            &mut out,
            &mut seen,
            QueryVariant {
                query,
                effects,
                required_commune: None,
            },
        );
    }

    for (seed, effects) in seeds {
        let mut prepared = vec![(seed, effects, None)];
        if let Some((value, mut fx)) = prepared
            .first()
            .and_then(|(s, fx, _)| strip_postcode_country_prefix(s).map(|v| (v, fx.clone())))
        {
            fx.push(Effect::PostcodePrefix);
            prepared.push((value, fx, None));
        }

        // The DE tables are deliberately separate from the generic rules.  Add
        // each successfully transformed stage, so combined inputs (country +
        // abbreviation + exonym) remain compositional without affecting any
        // non-DE query path.
        let mut staged = prepared.clone();
        for (effect, transform) in [
            (
                Effect::Country,
                strip_country_tokens as fn(&str) -> Option<String>,
            ),
            (Effect::AdminTail, strip_admin_tail),
            (Effect::Abbreviation, expand_de_abbreviation),
        ] {
            if let Some((value, mut fx, required)) = staged.last().and_then(|(s, fx, required)| {
                transform(s).map(|v| (v, fx.clone(), required.clone()))
            }) {
                fx.push(effect);
                staged.push((value, fx, required));
            }
        }
        if official_commune_alias_allowed {
            if let Some((value, mut fx, target)) = staged.last().and_then(|(s, fx, _)| {
                expand_de_official_commune_alias(s).map(|(v, target)| (v, fx.clone(), target))
            }) {
                fx.push(Effect::OfficialCommuneAlias);
                staged.push((value, fx, Some(target)));
            }
        }
        if let Some((value, mut fx, target)) = staged.last().and_then(|(s, fx, _)| {
            expand_de_city_alias(s).map(|(v, target)| (v, fx.clone(), target))
        }) {
            fx.push(Effect::CityAlias);
            staged.push((value, fx, Some(target)));
        }
        prepared = staged;

        // Apply a lost-zero retry and the two orthographic directions to every
        // structural seed.  This covers combined real inputs such as
        // `Caecilienstrasse ... 1067 Dresden` without an exponential search.
        let structural = prepared.clone();
        for (value, fx, required_commune) in structural {
            push_unique(
                &mut out,
                &mut seen,
                QueryVariant {
                    query: value.clone(),
                    effects: fx.clone(),
                    required_commune: required_commune.clone(),
                },
            );
            if let Some(padded) = pad_lost_postcode_zero(&value) {
                let mut pfx = fx.clone();
                pfx.push(Effect::PostcodeZero);
                push_unique(
                    &mut out,
                    &mut seen,
                    QueryVariant {
                        query: padded,
                        effects: pfx,
                        required_commune: required_commune.clone(),
                    },
                );
            }

            let collapsed = ascii_digraphs_to_german(&value);
            if collapsed != value {
                let mut ofx = fx.clone();
                ofx.push(Effect::Orthography);
                push_unique(
                    &mut out,
                    &mut seen,
                    QueryVariant {
                        query: collapsed,
                        effects: ofx,
                        required_commune: required_commune.clone(),
                    },
                );
            }
            let ascii = value.replace('ß', "ss");
            if ascii != value {
                let mut ofx = fx;
                ofx.push(Effect::Orthography);
                push_unique(
                    &mut out,
                    &mut seen,
                    QueryVariant {
                        query: ascii,
                        effects: ofx,
                        required_commune,
                    },
                );
            }
        }
    }
    out.truncate(20);
    out
}

fn german_types() -> Vec<&'static str> {
    crate::rules::rules()
        .de_street_types
        .iter()
        .map(String::as_str)
        .collect()
}

fn street_type_family(value: &str) -> Vec<&'static str> {
    if matches!(value, "straße" | "strasse" | "str") {
        german_types()
            .into_iter()
            .filter(|candidate| matches!(*candidate, "straße" | "strasse" | "str"))
            .collect()
    } else {
        german_types()
            .into_iter()
            .filter(|candidate| *candidate == value)
            .collect()
    }
}

/// Exact-key alternatives for compound German street types.  The caller adds
/// these only for a DE sheet and keeps the unmodified phrase first.
pub(crate) fn street_variants(phrase: &str) -> Vec<String> {
    let words: Vec<&str> = phrase.split_whitespace().collect();
    let types = german_types();
    if words.is_empty() || types.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    for (i, word) in words.iter().enumerate() {
        // A separated type (`Haupt Straße`) can be joined; Straße/Strasse/Str
        // are one equivalence family so an index spelling `Hauptstr.` is also
        // reachable from a query spelling `Hauptstraße`.
        if i > 0 && types.contains(word) {
            for replacement in street_type_family(word) {
                let mut v: Vec<String> = words.iter().map(|w| (*w).to_string()).collect();
                v[i - 1].push_str(replacement);
                v.remove(i);
                let candidate = v.join(" ");
                if candidate != phrase && seen.insert(candidate.clone()) {
                    out.push(candidate);
                }
            }
        }

        // A compound (`Kirschallee`, `Alsterufer`, `Bahnhofstr`) can be split,
        // or have the Straße family spelling swapped while remaining joined.
        for suffix in &types {
            let Some(prefix) = word.strip_suffix(suffix) else {
                continue;
            };
            if prefix.chars().count() < 2 {
                continue;
            }
            for replacement in street_type_family(suffix) {
                let mut joined: Vec<String> = words.iter().map(|w| (*w).to_string()).collect();
                joined[i] = format!("{prefix}{replacement}");
                let candidate = joined.join(" ");
                if candidate != phrase && seen.insert(candidate.clone()) {
                    out.push(candidate);
                }

                let mut split: Vec<String> = words.iter().map(|w| (*w).to_string()).collect();
                split.splice(i..=i, [prefix.to_string(), replacement.to_string()]);
                let candidate = split.join(" ");
                if candidate != phrase && seen.insert(candidate.clone()) {
                    out.push(candidate);
                }
            }
        }
    }
    #[cfg(test)]
    STREET_RUNTIME_TRACE.with(|trace| {
        if let Some(events) = trace.borrow_mut().as_mut() {
            events.extend(
                out.iter()
                    .cloned()
                    .map(|candidate| (phrase.to_string(), candidate)),
            );
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orthography_is_bidirectional_and_composes() {
        let ascii = query_variants("Caecilienstrasse 29-33 Koeln");
        assert!(ascii.iter().any(|v| v.query.contains("cacilienstraße")));
        let german = query_variants("München, Hanauer Straße 68");
        assert!(german.iter().any(|v| v.query.contains("muenchen")));
        assert!(german.iter().any(|v| v.query.contains("strasse")));
    }

    #[test]
    fn official_commune_alias_requires_an_exact_terminal_postcode_tail() {
        for (raw, expected, required_commune) in [
            (
                "Jacobistraße 37, 61348 Bad Homburg vor der Höhe",
                "jacobistraße 37 61348 bad homburg v d hohe",
                "bad homburg v d hohe",
            ),
            (
                "Auf der Steinkaut 1–15, 61352 Bad Homburg vor der Höhe",
                "auf der steinkaut 1 61352 bad homburg v d hohe",
                "bad homburg v d hohe",
            ),
            (
                "Dorotheenstr. 24, 61348 Bad Homburg vor der Höhe",
                "dorotheenstr 24 61348 bad homburg v d hohe",
                "bad homburg v d hohe",
            ),
            (
                "Domplatz 2, 65549 Limburg an der Lahn",
                "domplatz 2 65549 limburg a d lahn",
                "limburg a d lahn",
            ),
            (
                "Lispenhäuser Straße 41, 36199 Rotenburg an der Fulda",
                "lispenhauser straße 41 36199 rotenburg a d fulda",
                "rotenburg a d fulda",
            ),
            (
                "Rathausplatz 20, 67061 Ludwigshafen/Rhein",
                "rathausplatz 20 67061 ludwigshafen am rhein",
                "ludwigshafen am rhein",
            ),
            (
                "Luitpoldstr. 48, 67063 Ludwigshafen/Rhein",
                "luitpoldstr 48 67063 ludwigshafen am rhein",
                "ludwigshafen am rhein",
            ),
            (
                "Rottstr. 17, 67061 Ludwigshafen/Rhein",
                "rottstr 17 67061 ludwigshafen am rhein",
                "ludwigshafen am rhein",
            ),
        ] {
            let variants = query_variants(raw);
            assert!(
                variants.iter().any(|variant| {
                    variant.query == expected
                        && variant.required_commune.as_deref() == Some(required_commune)
                }),
                "OFFICIAL_COMMUNE_ALIAS_OBSERVER: {raw}: {variants:#?}"
            );
        }

        for untouched in [
            "Jacobistraße 37, Bad Homburg vor der Höhe",
            "Jacobistraße 37, 6134 Bad Homburg vor der Höhe",
            "Jacobistraße 37, 61348 Bad Homburg vor der Höhe Empfang",
            "Bad Homburg vor der Höhe, Jacobistraße 37, 61348",
            "Jacobistraße 37, 66424 Homburg (Saar)",
            "Domplatz 2, 67117 Limburgerhof",
            "Domplatz 2, 65549 Limburg-Weilburg",
            "Markt 1, 27356 Rotenburg (Wümme)",
            "Rathausplatz 20, Ludwigshafen/Rhein",
            "Rathausplatz 20, 6706 Ludwigshafen/Rhein",
            "Rathausplatz 20, 67061 Ludwigshafen/Rhein Empfang",
            "Ludwigshafen/Rhein, Rathausplatz 20, 67061",
            "Bismarckstr. 44/48, 67059 Ludwigshafen/Rhein",
        ] {
            assert!(
                query_variants(untouched)
                    .iter()
                    .all(|variant| variant.required_commune.as_deref()
                        != Some("bad homburg v d hohe")
                        && variant.required_commune.as_deref() != Some("limburg a d lahn")
                        && variant.required_commune.as_deref() != Some("rotenburg a d fulda")
                        && variant.required_commune.as_deref() != Some("ludwigshafen am rhein")),
                "{untouched} must not enter the official commune alias path"
            );
        }
    }

    #[test]
    fn delivery_noise_variants_require_exact_raw_comma_segments() {
        for (raw, expected, effect) in [
            (
                "z. Hd. Empfang, Schloßbergring 2, 79098 Freiburg/Breisgau",
                "schloßbergring 2 79098 freiburg breisgau",
                Effect::RecipientPrefix,
            ),
            (
                "Trankgasse 11, 50667 Köln, Hinterhaus 1. OG",
                "trankgasse 11 50667 koln",
                Effect::SubaddressTail,
            ),
            (
                "Hans-Striegelski-Straße 5, 15562 Rüdersdorf bei Berlin, Aufgang B 3. OG",
                "hans striegelski straße 5 15562 rudersdorf bei berlin",
                Effect::SubaddressTail,
            ),
            (
                "Museumstraße 23, 22765 Hamburg, 2. OG",
                "museumstraße 23 22765 hamburg",
                Effect::SubaddressTail,
            ),
            (
                "Markt 7, 06618 Naumburg, Erdgeschoss",
                "markt 7 06618 naumburg",
                Effect::SubaddressTail,
            ),
        ] {
            let variants = query_variants(raw);
            assert!(
                variants.iter().any(|variant| {
                    variant.query == expected && variant.effects.contains(&effect)
                }),
                "{raw}: {variants:#?}"
            );
        }

        for untouched in [
            "An die Poststelle 7, 10115 Berlin",
            "Hinterhaus 1, 10115 Berlin",
            "Erdgeschoss 2, 10115 Berlin",
            "Museum, Platz 1, 10115 Berlin",
        ] {
            assert!(
                query_variants(untouched).iter().all(|variant| {
                    !variant.effects.iter().any(|effect| {
                        matches!(effect, Effect::RecipientPrefix | Effect::SubaddressTail)
                    })
                }),
                "{untouched} must remain ordinary source text"
            );
        }
    }

    #[test]
    fn parenthetical_subaddress_requires_the_exact_typed_raw_shape() {
        for (raw, expected, commune) in [
            (
                "Unter den Eichen 2 (Uhle-Hof), 29690 Schwarmstedt",
                "Unter den Eichen 2, 29690 Schwarmstedt",
                "schwarmstedt",
            ),
            (
                "Am Markt 4 (Rathaus), 27404 Zeven",
                "Am Markt 4, 27404 Zeven",
                "zeven",
            ),
            (
                "Kopernikusstr. 16 (Bauteil Ost Verfügungszentrum), 52074 Aachen",
                "Kopernikusstr. 16, 52074 Aachen",
                "aachen",
            ),
            (
                "Universitätsstr. 105 (Raum 2.22), 44801 Bochum",
                "Universitätsstr. 105, 44801 Bochum",
                "bochum",
            ),
            (
                "Kohlweg 7 (Villa Europa), 66123 Saarbrücken",
                "Kohlweg 7, 66123 Saarbrücken",
                "saarbrucken",
            ),
            (
                "Bahnhofstraße 1 (im Schloss), 78713 Schramberg",
                "Bahnhofstraße 1, 78713 Schramberg",
                "schramberg",
            ),
            (
                "Rheinstraße 45/46 (Aufgang 6), 12161 Berlin",
                "Rheinstraße 45/46, 12161 Berlin",
                "berlin",
            ),
            (
                "Alaunplatz 3b-3c (Haus), 01099 Dresden",
                "Alaunplatz 3b-3c, 01099 Dresden",
                "dresden",
            ),
        ] {
            assert_eq!(
                parenthetical_subaddress_raw(raw),
                Some((expected.to_string(), commune.to_string())),
                "PARENTHETICAL_SUBADDRESS_PARSER_OBSERVER: {raw}"
            );
            assert!(query_variants(raw).iter().any(|variant| {
                variant.query == expected
                    && variant.effects.contains(&Effect::ParentheticalSubaddress)
            }));
        }
    }

    #[test]
    fn parenthetical_subaddress_rejects_the_complete_negative_surface() {
        let oversized = format!("Kohlweg 7 ({}), 66123 Saarbrücken", "a".repeat(81));
        let negatives = [
            "Kohlweg 7 (Villa (Europa)), 66123 Saarbrücken",
            "Kohlweg 7 (Villa Europa, 66123 Saarbrücken",
            "Kohlweg 7 Villa Europa), 66123 Saarbrücken",
            "Kohlweg (Villa Europa) 7, 66123 Saarbrücken",
            "Kohlweg 7(Villa Europa), 66123 Saarbrücken",
            "Kohlweg 7 ä (Villa Europa), 66123 Saarbrücken",
            "Kohlweg 3ä (Villa Europa), 66123 Saarbrücken",
            "Kohlweg 123456 (Villa Europa), 66123 Saarbrücken",
            "Kohlweg 3-5/7 (Villa Europa), 66123 Saarbrücken",
            "7 (Villa Europa), 66123 Saarbrücken",
            "Kohlweg 7 (), 66123 Saarbrücken",
            "Kohlweg 7 (123), 66123 Saarbrücken",
            "Kohlweg 7 (Villa Europa) 66123 Saarbrücken",
            "Kohlweg 7 (Villa Europa), 6612 Saarbrücken",
            "Kohlweg 7 (Villa Europa), 66123",
            "Kohlweg 7 (Villa Europa), 66123 123",
            "Kohlweg 7 (Villa Europa), 66123 Saarbrücken, Deutschland",
            "Kohlweg 7 (Villa Europa), 66123 Saarbrücken 2",
            "Postfach 7 (Villa Europa), 66123 Saarbrücken",
            "PO Box 7 (Villa Europa), 66123 Saarbrücken",
            "Oberfrohnaer Straße 129; 131 (zwischen), 09117 Chemnitz",
            "Kohlweg 7 (Villa Europa),\t66123 Saarbrücken",
        ];
        for raw in negatives
            .into_iter()
            .chain(std::iter::once(oversized.as_str()))
        {
            assert_eq!(
                parenthetical_subaddress_raw(raw),
                None,
                "PARENTHETICAL_SUBADDRESS_FAIL_CLOSED_OBSERVER: {raw}"
            );
            assert!(query_variants(raw)
                .iter()
                .all(|variant| !variant.effects.contains(&Effect::ParentheticalSubaddress)));
        }
    }

    #[test]
    fn three_field_address_selection_is_exact_bounded_and_additive() {
        for (raw, expected) in [
            (
                "Bergheimer Straße 147, Gebäude C, 69115 Heidelberg",
                "Bergheimer Straße 147, 69115 Heidelberg",
            ),
            (
                "Schloss Mosigkau, Knobelsdorffallee 2-3, 06847 Dessau-Roßlau",
                "Knobelsdorffallee 2-3, 06847 Dessau-Roßlau",
            ),
            (
                "Eigenbetrieb Kloster Chorin, Amt Chorin 11 A, 16230 Chorin",
                "Amt Chorin 11 A, 16230 Chorin",
            ),
        ] {
            assert_eq!(
                select_three_field_address_raw(raw).as_deref(),
                Some(expected)
            );
            assert!(query_variants(raw).iter().any(|variant| {
                variant.query == expected && variant.effects.contains(&Effect::AddressField)
            }));
        }

        for raw in [
            "Wilstorfer Straße 71, Tor 2, 21073 Hamburg",
            "Rathausstr. 1, Rheingoldstr. 14, 68199 Mannheim",
            "R 5, 6-13, 68161 Mannheim",
            "N 7, 18, 68161 Mannheim",
            "Campus, Gebäude A4 2, 66123 Saarbrücken",
            "Campus, Gebäude A1.3, 66123 Saarbrücken",
            "Flugplatz Rügen, Güttin, 18573 Dreschvitz",
            "Flugplatz Rügen, Güttin 66, 1857 Dreschvitz",
            "Flugplatz Rügen, Güttin 66, 18573 Ort 2",
            "POI, Güttin 66, 18573 Dreschvitz, Deutschland",
        ] {
            assert_eq!(select_three_field_address_raw(raw), None, "{raw}");
            assert!(query_variants(raw)
                .iter()
                .all(|variant| !variant.effects.contains(&Effect::AddressField)));
        }
    }

    #[test]
    fn missing_postcode_comma_requires_one_exact_unambiguous_boundary() {
        for (raw, expected) in [
            (
                "Hauptstraße 32   23769 Landkirchen",
                "Hauptstraße 32, 23769 Landkirchen",
            ),
            (
                "Burghof 9   14776 Brandenburg an der Havel",
                "Burghof 9, 14776 Brandenburg an der Havel",
            ),
            (
                "August-Sonntag-Straße 5   14770 Brandenburg an der Havel",
                "August-Sonntag-Straße 5, 14770 Brandenburg an der Havel",
            ),
            ("Kirchgasse 5   12043 Berlin", "Kirchgasse 5, 12043 Berlin"),
        ] {
            assert_eq!(
                restore_missing_postcode_comma_raw(raw).as_deref(),
                Some(expected)
            );
            let variants = query_variants(raw);
            assert!(variants.iter().any(|variant| {
                variant.query == expected
                    && variant
                        .effects
                        .contains(&Effect::MissingCommaPostcodeBoundary)
            }));
        }

        for untouched in [
            "August-Keiler-Straße   34",
            "Hirnerweg   15",
            "Arena-Straße   1",
            "Fürstenbergstraße 13 - 15   48147 Münster",
            "Auf der Burg 1 und 3   65817 Eppstein",
            "Max-Brauer-Allee 83 – 85   22765 Hamburg",
            "Carl-Zeiss-Stiftung – Geschäftsstelle   Breitscheidstraße 10   70174 Stuttgart",
            "c/o Universitätsbibliothek   Zeitschriftenabteilung / e-journals   Otto-Behaghel-Strasse 8   35394 Gießen",
            "Haid-und-Neu-Str. 9   76131 Karlsruhe",
            "Kirchgasse 5    12043 Berlin",
            "Kirchgasse 5\t\t\t12043 Berlin",
            "Kirchgasse 5, 12043 Berlin",
        ] {
            assert_eq!(restore_missing_postcode_comma_raw(untouched), None, "{untouched}");
            assert!(query_variants(untouched).iter().all(|variant| !variant
                .effects
                .contains(&Effect::MissingCommaPostcodeBoundary)));
        }
    }

    #[test]
    fn exact_delivery_cleanup_preserves_raw_comma_boundaries_before_the_global_budget() {
        for (raw, expected, effect) in [
            (
                "für den Empfang, Berliner Straße 121, 13187 Berlin",
                "Berliner Straße 121, 13187 Berlin",
                Effect::RecipientPrefix,
            ),
            (
                "An die Poststelle, Schloßstraße 48, 12165 Berlin",
                "Schloßstraße 48, 12165 Berlin",
                Effect::RecipientPrefix,
            ),
            (
                "Breite Str. 49, 23769 Burg auf Fehmarn, Tel. 030 49499637",
                "Breite Str. 49, 23769 Burg auf Fehmarn",
                Effect::SubaddressTail,
            ),
            (
                "Berlin, Friedrichstraße 55, 10117",
                "Friedrichstraße 55, 10117 Berlin",
                Effect::LocalityFirst,
            ),
        ] {
            let variants = query_variants(raw);
            assert!(
                variants.iter().any(|variant| {
                    variant.query == expected && variant.effects.contains(&effect)
                }),
                "the exact structural cleanup must not be truncated for {raw}: {variants:#?}"
            );
        }
    }

    #[test]
    fn recipient_cleanup_composes_with_street_type_abbreviation_before_the_budget() {
        let variants = query_variants("für den Empfang, Max-Dohrn-Str. 5");
        assert!(
            variants.iter().any(|variant| {
                variant.query == "max dohrn straße 5"
                    && variant.effects.contains(&Effect::RecipientPrefix)
                    && variant.effects.contains(&Effect::Abbreviation)
            }),
            "the exact dirty-corpus composition must survive the bounded variant cap: {variants:#?}"
        );
    }

    #[test]
    fn locality_first_variant_requires_three_fields_and_terminal_postcode() {
        let raw = "Frankfurt am Main, Domstraße 10, 60311";
        let variants = query_variants(raw);
        assert!(
            variants.iter().any(|variant| {
                variant.query == "domstraße 10 60311 frankfurt am main"
                    && variant.effects.contains(&Effect::LocalityFirst)
            }),
            "{variants:#?}"
        );

        for untouched in [
            "Domstraße 10, 60311 Frankfurt am Main",
            "Museum, Platz 1, 10115 Berlin",
            "Berlin, Straße des 17. Juni, 10623",
            "Berlin, Alexanderplatz, 10178",
        ] {
            assert!(query_variants(untouched)
                .iter()
                .all(|variant| !variant.effects.contains(&Effect::LocalityFirst)));
        }
    }

    #[test]
    fn short_street_type_expansion_requires_house_context() {
        for (raw, expected) in [
            (
                "Wilhelm-Lückert-str. 4, 10115 Berlin",
                "wilhelm luckert straße 4 10115 berlin",
            ),
            (
                "Proraer al. 119, 18609 Binz",
                "proraer allee 119 18609 binz",
            ),
            ("Uber pl. 1, 10243 Berlin", "uber platz 1 10243 berlin"),
        ] {
            let variants = query_variants(raw);
            assert!(
                variants.iter().any(|variant| {
                    variant.query == expected && variant.effects.contains(&Effect::Abbreviation)
                }),
                "{raw}: {variants:#?}"
            );
        }
        for (raw, expected) in [
            ("Messepl. 2, 45131 Essen", "messeplatz 2 45131 essen"),
            (
                "Joachimspl. 1 – 3, 16247 Joachimsthal",
                "joachimsplatz 1 – 3 16247 joachimsthal",
            ),
            (
                "Chemnitz, Theaterpl. 1, 09111",
                "theaterplatz 1 09111 chemnitz",
            ),
        ] {
            let variants = query_variants(raw);
            assert!(
                variants.iter().any(|variant| {
                    variant.query == expected && variant.effects.contains(&Effect::Abbreviation)
                }),
                "attached Platz abbreviation must expand for {raw}: {variants:#?}"
            );
        }
        for untouched in [
            "str der einheit 4",
            "al capone 4",
            "pl der einheit",
            "g sieben berlin",
            "Messepl 2, 45131 Essen",
            "Messepl. Essen",
        ] {
            assert!(query_variants(untouched)
                .iter()
                .all(|variant| !variant.effects.contains(&Effect::Abbreviation)));
        }
    }

    #[test]
    fn structural_variants_preserve_parent_and_range_suffix() {
        // These checks require the real rule tables to be current in the caller;
        // admin-tail behavior is covered end-to-end with an embedded rules section.
        assert!(query_variants("Cäcilienstraße 29 bis 33 Köln")
            .iter()
            .any(|v| v.query.contains("29 33")));
        assert!(query_variants("Stauffenbergstraße 13-14 D-10785 Berlin")
            .iter()
            .any(|v| v.query.contains("10785")));
        assert!(query_variants("Sophienstraße 1067 Dresden")
            .iter()
            .any(|v| v.query.contains("01067")));
        assert!(query_variants("Hauptstraße 17/2")
            .iter()
            .any(|v| v.effects.contains(&Effect::HouseSlash)));
        for not_shorthand in [
            "Steinstraße 80/82/84",
            "Briennerstraße 7/8",
            "Viktoriastraße 1 1/3",
            "Bahnhofstraße 30 bis 30/1 70372 Stuttgart",
        ] {
            assert!(
                query_variants(not_shorthand)
                    .iter()
                    .all(|v| !v.effects.contains(&Effect::HouseSlash)),
                "{not_shorthand} is not a 17/2-style shorthand"
            );
        }
    }

    #[test]
    fn postal_tail_eligibility_uses_untruncated_structural_effects() {
        assert!(postal_tail_eligible("Postallee 20 12345 Unbekannt"));
        assert!(!postal_tail_eligible("Postallee 20-38 12345 Unbekannt"));
        assert!(!postal_tail_eligible("Postallee 17/2 12345 Unbekannt"));
        assert!(postal_tail_eligible("Postallee 80/82/84 12345 Unbekannt"));

        let rules_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../rules");
        let entries = crate::rules::entries_from_tsv_dir(&rules_dir).unwrap();
        let section = crate::rules::serialize_entries(&entries);
        let installed = crate::rules::from_section(&section);
        let _scope = crate::rules::scope(installed);
        let crowded = "Cäcilienstraße 29-33 1067 Dresden D-50667 A M Cologne OT Prora Deutschland";
        assert!(!postal_tail_eligible(crowded));
        let bounded = query_variants(crowded);
        assert!(
            bounded.iter().all(|variant| !variant
                .effects
                .iter()
                .any(|effect| matches!(*effect, Effect::HouseRange | Effect::HouseSlash))),
            "fixture must prove that the bounded variant list can lose the structural effect: {bounded:#?}"
        );
    }

    #[test]
    fn letter_suffixed_house_ranges_keep_the_first_house_and_freeze_postal_tail() {
        for (raw, first_house, full_range) in [
            (
                "Alaunplatz 3b-3c, 01099 Dresden",
                "alaunplatz 3b 01099 dresden",
                "alaunplatz 3b 3c 01099 dresden",
            ),
            (
                "Hauptstraße 1a-35, 01097 Dresden",
                "hauptstraße 1a 01097 dresden",
                "hauptstraße 1a 35 01097 dresden",
            ),
            (
                "Wiener Platz 4d-4g, 01069 Dresden",
                "wiener platz 4d 01069 dresden",
                "wiener platz 4d 4g 01069 dresden",
            ),
            (
                "Musterweg 11–13c, 01067 Dresden",
                "musterweg 11 01067 dresden",
                "musterweg 11 13c 01067 dresden",
            ),
            (
                "Testweg 3b—5, 01067 Dresden",
                "testweg 3b 01067 dresden",
                "testweg 3b 5 01067 dresden",
            ),
        ] {
            let variants = query_variants(raw);
            assert!(
                variants.iter().any(|variant| {
                    variant.query == first_house && variant.effects.contains(&Effect::HouseRange)
                }),
                "{raw}: a range retry must truncate to the first source endpoint: {variants:#?}"
            );
            assert!(
                variants.iter().any(|variant| {
                    variant.query == full_range
                        && variant.effects.contains(&Effect::HouseRange)
                }),
                "{raw}: the full source range must remain an explicit structural retry: {variants:#?}"
            );
            assert!(
                !postal_tail_eligible(raw),
                "{raw}: a source house range must not enter the independent postal-tail rule"
            );
        }
    }

    #[test]
    fn letter_house_range_scope_stays_compact_and_bounded() {
        for raw in [
            "Testweg 3b - 3c, 01067 Dresden",
            "Testweg 3b bis 3c, 01067 Dresden",
            "Testweg C-5, 01067 Dresden",
            "Testweg 3b-c, 01067 Dresden",
            "Testweg 12345a-12345b, 01067 Dresden",
        ] {
            assert!(
                postal_tail_eligible(raw),
                "{raw}: syntax outside the authorized compact range grammar must remain untouched"
            );
        }
    }
}
