//! Rules-in-data: curated tables (commune/city aliases, abbreviations, street-type
//! words, capital anchors, stop lists) can be embedded in the data file
//! (SEC_RULES section); the constants below are the built-in defaults used when
//! a file carries no rules. Updating a rule means rebuilding the data, not
//! releasing a new engine. When both exist, the section wins.
//!
//! Rules belong to the data file that carries them, not to the process: opening
//! two files built from different rule tables (say a current sheet and an older
//! one) must not make either answer with the other's rules. Each index therefore
//! owns its rule set and makes it current for the thread while it serves a query.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::OnceLock;

// section entry classes (u8) and their TSV file names
pub const CLASSES: &[(u8, &str)] = &[
    (1, "commune_alias"),       // key → value
    (2, "city_alias"),          // key → value
    (3, "abbrev2"),             // key="a b" → full word
    (4, "capitals"),            // list
    (5, "street_types_cyr"),    // list (type padding/rotation — Cyrillic)
    (16, "street_types_latin"), // list (type padding — Latin)
    (17, "street_types_extra"), // list (type recognition only: nl/rs/uz…)
    (6, "affix_extra"),         // list
    (7, "noise"),               // list
    (8, "noise_after"),         // list
    (9, "region_markers"),      // list
    (10, "countries_mid"),      // list
    (11, "countries_tail"),     // list
    (12, "place_junk"),         // list
    (13, "place_prefix"),       // list
    (14, "fr_ord_cities"),      // list
    (15, "place_type_strip"),   // list
];

pub struct Rules {
    pub commune_alias: HashMap<String, String>,
    pub city_alias: Vec<(String, String)>,
    pub abbrev2: Vec<(String, String)>, // ("a b", "full")
    pub capitals: Vec<String>,
    pub types_cyr: Vec<String>,
    pub types_latin: Vec<String>,
    pub types_extra: HashSet<String>,
    pub affix_extra: HashSet<String>,
    pub noise: HashSet<String>,
    pub noise_after: HashSet<String>,
    pub region_markers: HashSet<String>,
    pub countries_mid: HashSet<String>,
    pub countries_tail: HashSet<String>,
    pub place_junk: HashSet<String>,
    pub place_prefix: HashSet<String>,
    pub fr_ord_cities: HashSet<String>,
    pub place_type_strip: HashSet<String>,
}

impl Rules {
    pub fn is_street_type(&self, w: &str) -> bool {
        self.types_extra.contains(w)
            || self.types_cyr.iter().any(|t| t == w)
            || self.types_latin.iter().any(|t| t == w)
    }
    pub fn is_affix(&self, w: &str) -> bool {
        self.is_street_type(w) || self.affix_extra.contains(w)
    }
    pub fn commune_alias(&self, name: &str) -> Option<&str> {
        self.commune_alias.get(name).map(|s| s.as_str())
    }

    fn add(&mut self, class: u8, key: String, val: String) {
        match class {
            1 => {
                self.commune_alias.insert(key, val);
            }
            2 => self.city_alias.push((key, val)),
            3 => self.abbrev2.push((key, val)),
            4 => self.capitals.push(key),
            5 => self.types_cyr.push(key),
            16 => self.types_latin.push(key),
            17 => {
                self.types_extra.insert(key);
            }
            6 => {
                self.affix_extra.insert(key);
            }
            7 => {
                self.noise.insert(key);
            }
            8 => {
                self.noise_after.insert(key);
            }
            9 => {
                self.region_markers.insert(key);
            }
            10 => {
                self.countries_mid.insert(key);
            }
            11 => {
                self.countries_tail.insert(key);
            }
            12 => {
                self.place_junk.insert(key);
            }
            13 => {
                self.place_prefix.insert(key);
            }
            14 => {
                self.fr_ord_cities.insert(key);
            }
            15 => {
                self.place_type_strip.insert(key);
            }
            _ => {}
        }
    }
}

// ===== built-in defaults (fallback used when no index carries a SEC_RULES section) =====

const D_COMMUNE_ALIAS: &[(&str, &str)] = &[
    ("den haag", "s gravenhage"),
    ("den bosch", "s hertogenbosch"),
    ("reggio emilia", "reggio nell emilia"),
];

const D_CITY_ALIASES: &[(&str, &str)] = &[
    ("спб", "санкт петербург"),
    ("мск", "москва"),
    ("екб", "екатеринбург"),
    ("нск", "новосибирск"),
    ("питер", "санкт петербург"),
    ("bg", "beograd"),
    ("ns", "novi sad"),
    ("amsterdam zo", "amsterdam"),
    ("moscow", "москва"),
    ("saint petersburg", "санкт петербург"),
    ("st petersburg", "санкт петербург"),
    ("rome", "roma"),
    ("milan", "milano"),
    ("naples", "napoli"),
    ("turin", "torino"),
    ("florence", "firenze"),
    ("venice", "venezia"),
    ("genoa", "genova"),
    ("padua", "padova"),
];

const D_ABBREV2: &[(&str, &str)] = &[
    ("p zza", "piazza"),
    ("c so", "corso"),
    ("v le", "viale"),
    ("пр кт", "проспект"),
    ("б р", "бульвар"),
    ("пр т", "проспект"),
];

const D_CAPITALS: &[&str] = &[
    "москва",
    "paris",
    "roma",
    "amsterdam",
    "beograd",
    "toshkent",
    "ташкент",
    "s gravenhage",
    "санкт петербург",
];

const D_TYPES_CYR: &[&str] = &[
    "улица",
    "проспект",
    "переулок",
    "набережная",
    "шоссе",
    "бульвар",
    "площадь",
    "проезд",
    "аллея",
    "тупик",
    "квартал",
    "микрорайон",
];
const D_TYPES_LATIN: &[&str] = &[
    // fr
    "rue",
    "avenue",
    "boulevard",
    "place",
    "impasse",
    "allee",
    "chemin",
    "route",
    "cours",
    "quai",
    "passage",
    "square",
    // it
    "via",
    "viale",
    "corso",
    "piazza",
    "largo",
    "vicolo",
    "strada",
];
const D_TYPES_EXTRA: &[&str] = &[
    // nl
    "straat",
    "laan",
    "weg",
    "plein",
    "gracht",
    "dijk",
    "kade",
    "steeg",
    "singel",
    "hof",
    // rs
    "ulica",
    "bulevar",
    "trg",
    "put",
    // uz (apostrophe removed by normalization: ko'chasi→kochasi)
    "kochasi",
    "kocha",
    "kuchasi",
    "mavze",
    "mavzesi",
    "dahasi",
    "daha",
    "prospekti",
    // it extras
    "piazzale",
    "salita",
];

const D_AFFIX_EXTRA: &[&str] = &[
    "массив",
    "massiv",
    "мкр",
    "микрорайон",
    "квартал",
    "kvartal",
    "жилой",
    "residential",
    "комплекс",
    "complex",
    "жк",
];

const D_NOISE: &[&str] = &["г", "гор", "город", "gorod", "shahri", "sh"];
const D_NOISE_AFTER: &[&str] = &["город", "grad", "sh"];

const D_REGION_MARKERS: &[&str] = &[
    "обл",
    "область",
    "области",
    "край",
    "края",
    "респ",
    "республика",
    "район",
    "района",
    "р н",
    "tumani",
    "tuman",
    "viloyat",
    "viloyati",
];

const D_COUNTRIES_MID: &[&str] = &[
    "france",
    "francia",
    "frankrijk",
    "франция",
    "italia",
    "italy",
    "italie",
    "италия",
    "srbija",
    "serbia",
    "сербия",
    "србија",
    "россия",
    "russia",
    "рф",
    "узбекистан",
    "uzbekistan",
    "ozbekiston",
    "nederland",
    "netherlands",
    "holland",
    "нидерланды",
];

const D_COUNTRIES_TAIL: &[&str] = &[
    "france",
    "francia",
    "франция",
    "nederland",
    "netherlands",
    "holland",
    "нидерланды",
    "italia",
    "italy",
    "italie",
    "италия",
    "srbija",
    "serbia",
    "сербия",
    "србија",
    "россия",
    "russia",
    "рф",
    "федерация",
    "российская",
    "uzbekistan",
    "узбекистан",
    "ozbekiston",
    "europe",
    "европа",
];

const D_PLACE_JUNK: &[&str] = &[
    "территория",
    "тер",
    "муниципальный",
    "округ",
    "поселение",
    "городской",
    "деревня",
    "дер",
    "село",
    "поселок",
    "пос",
    "рп",
    "пгт",
    "аул",
    "кишлак",
];

const D_PLACE_PREFIX: &[&str] = &[
    "снт",
    "днп",
    "тсн",
    "сдт",
    "ст",
    "садовое",
    "дачное",
    "некоммерческое",
    "товарищество",
    "массив",
    "микрорайон",
    "мкр",
    "квартал",
    "город",
    "обл",
    "область",
    "край",
    "район",
];

const D_FR_ORD_CITIES: &[&str] = &["paris", "lyon", "marseille"];

const D_PLACE_TYPE_STRIP: &[&str] = &[
    "квартал",
    "kvartal",
    "mavze",
    "mavzesi",
    "даха",
    "daha",
    "dahasi",
    "массив",
    "massiv",
    "мкр",
    "микрорайон",
];

fn defaults() -> Rules {
    let mut r = Rules {
        commune_alias: HashMap::new(),
        city_alias: Vec::new(),
        abbrev2: Vec::new(),
        capitals: Vec::new(),
        types_cyr: Vec::new(),
        types_latin: Vec::new(),
        types_extra: HashSet::new(),
        affix_extra: HashSet::new(),
        noise: HashSet::new(),
        noise_after: HashSet::new(),
        region_markers: HashSet::new(),
        countries_mid: HashSet::new(),
        countries_tail: HashSet::new(),
        place_junk: HashSet::new(),
        place_prefix: HashSet::new(),
        fr_ord_cities: HashSet::new(),
        place_type_strip: HashSet::new(),
    };
    for (k, v) in D_COMMUNE_ALIAS {
        r.add(1, k.to_string(), v.to_string());
    }
    for (k, v) in D_CITY_ALIASES {
        r.add(2, k.to_string(), v.to_string());
    }
    for (k, v) in D_ABBREV2 {
        r.add(3, k.to_string(), v.to_string());
    }
    let lists: &[(u8, &[&str])] = &[
        (4, D_CAPITALS),
        (5, D_TYPES_CYR),
        (16, D_TYPES_LATIN),
        (17, D_TYPES_EXTRA),
        (6, D_AFFIX_EXTRA),
        (7, D_NOISE),
        (8, D_NOISE_AFTER),
        (9, D_REGION_MARKERS),
        (10, D_COUNTRIES_MID),
        (11, D_COUNTRIES_TAIL),
        (12, D_PLACE_JUNK),
        (13, D_PLACE_PREFIX),
        (14, D_FR_ORD_CITIES),
        (15, D_PLACE_TYPE_STRIP),
    ];
    for (class, words) in lists {
        for w in *words {
            r.add(*class, w.to_string(), String::new());
        }
    }
    r
}

static FALLBACK: OnceLock<Rules> = OnceLock::new();

thread_local! {
    /// The rule set serving the query currently running on this thread.
    static CURRENT: Cell<Option<&'static Rules>> = const { Cell::new(None) };
}

/// Built-in defaults, used by files that carry no rule section.
pub fn defaults_static() -> &'static Rules {
    FALLBACK.get_or_init(defaults)
}

/// Restores the previously current rule set when dropped.
pub struct Scope(Option<&'static Rules>);

impl Drop for Scope {
    fn drop(&mut self) {
        CURRENT.with(|c| c.set(self.0));
    }
}

/// Make `r` the current rule set for this thread until the guard is dropped.
/// Nested scopes (a cascade querying a second index) restore correctly.
#[must_use]
pub fn scope(r: &'static Rules) -> Scope {
    CURRENT.with(|c| Scope(c.replace(Some(r))))
}

/// Rules serving the running query: the current index's own set, else the defaults.
pub fn rules() -> &'static Rules {
    CURRENT.with(|c| c.get()).unwrap_or_else(defaults_static)
}

/// The rule set carried by one index section. A file whose section is absent, empty or
/// truncated falls back to the built-in defaults rather than to a partial rule set.
/// The returned reference lives as long as the process, like the index's mapping.
pub fn from_section(bytes: &[u8]) -> &'static Rules {
    if bytes.len() < 4 {
        return defaults_static();
    }
    let declared = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
    let entries = parse_entries(bytes);
    if entries.len() != declared || entries.is_empty() {
        return defaults_static();
    }
    Box::leak(Box::new(rules_from_entries(entries)))
}

/// Like `from_section`, but returns an OWNED `Box<Rules>` (or None for no/invalid section) instead
/// of leaking, so the caller can store it in the Index and free it on Drop.
pub fn from_section_owned(bytes: &[u8]) -> Option<Box<Rules>> {
    if bytes.len() < 4 {
        return None;
    }
    let declared = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
    let entries = parse_entries(bytes);
    if entries.len() != declared || entries.is_empty() {
        return None;
    }
    Some(Box::new(rules_from_entries(entries)))
}

/// Parse raw section entries (used by the watermark verifier).
pub fn parse_entries(bytes: &[u8]) -> Vec<(u8, String, String)> {
    let mut out = Vec::new();
    if bytes.len() < 4 {
        return out;
    }
    let n = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
    let mut p = 4usize;
    for _ in 0..n {
        if p + 3 > bytes.len() {
            return out;
        }
        let class = bytes[p];
        let kl = u16::from_le_bytes(bytes[p + 1..p + 3].try_into().unwrap()) as usize;
        p += 3;
        if p + kl + 2 > bytes.len() {
            return out;
        }
        let key = String::from_utf8_lossy(&bytes[p..p + kl]).into_owned();
        p += kl;
        let vl = u16::from_le_bytes(bytes[p..p + 2].try_into().unwrap()) as usize;
        p += 2;
        if p + vl > bytes.len() {
            return out;
        }
        out.push((
            class,
            key,
            String::from_utf8_lossy(&bytes[p..p + vl]).into_owned(),
        ));
        p += vl;
    }
    out
}

/// Build the active rule set from raw entries: defaults extended by the entries, applied
/// in canonical (sorted) order so the result never depends on the order they arrive in.
/// Some classes are applied as ordered lists (alias and abbreviation rewrites are
/// sequential), so sorting keeps any two sheets carrying the same rule set behaving
/// identically, whatever order the entries were written in.
fn rules_from_entries(mut entries: Vec<(u8, String, String)>) -> Rules {
    entries.sort();
    let mut r = defaults(); // the section EXTENDS the defaults (key/value pairs override by key)
    for (class, key, val) in entries {
        r.add(class, key, val);
    }
    r
}

/// Serialize entries for the builder: u32 n, then (class u8, klen u16, key, vlen u16, val)*.
pub fn serialize_entries(entries: &[(u8, String, String)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for (class, key, val) in entries {
        out.push(*class);
        let kb = key.as_bytes();
        let vb = val.as_bytes();
        out.extend_from_slice(&(kb.len().min(65535) as u16).to_le_bytes());
        out.extend_from_slice(&kb[..kb.len().min(65535)]);
        out.extend_from_slice(&(vb.len().min(65535) as u16).to_le_bytes());
        out.extend_from_slice(&vb[..vb.len().min(65535)]);
    }
    out
}

/// Read rule TSV files from a directory (builder input): file name maps to class,
/// columns are tab-separated. For the 3-column abbrev2.tsv the key is "a b"
/// (first two columns joined by a space).
pub fn entries_from_tsv_dir(dir: &Path) -> std::io::Result<Vec<(u8, String, String)>> {
    let mut out = Vec::new();
    for (class, name) in CLASSES {
        let p = dir.join(format!("{name}.tsv"));
        if !p.exists() {
            continue;
        }
        for line in std::fs::read_to_string(&p)?.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let cols: Vec<&str> = line.split('\t').collect();
            let (key, val) = match (*name, cols.len()) {
                ("abbrev2", 3..) => (format!("{} {}", cols[0], cols[1]), cols[2].to_string()),
                (_, 2..) => (cols[0].to_string(), cols[1].to_string()),
                _ => (cols[0].to_string(), String::new()),
            };
            out.push((*class, key, val));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entries() -> Vec<(u8, String, String)> {
        let s = |x: &str| x.to_string();
        vec![
            (2, s("spb"), s("sankt peterburg")),
            (2, s("bg"), s("beograd")),
            (3, s("p zza"), s("piazza")),
            (3, s("c so"), s("corso")),
            (4, s("paris"), String::new()),
            (4, s("roma"), String::new()),
            (5, s("ulica"), String::new()),
            (16, s("rue"), String::new()),
            (17, s("straat"), String::new()),
        ]
    }

    /// SEC_RULES entries can reach the engine in any order: the section is written by
    /// several producers and its on-disk order is not part of the format contract. Alias
    /// and abbreviation rewrites are applied as ordered lists, so the INSTALLED rule set
    /// must not depend on arrival order — otherwise two sheets carrying the same rules
    /// could answer differently.
    #[test]
    fn rule_install_does_not_depend_on_entry_arrival_order() {
        let base = rules_from_entries(sample_entries());
        for rotate in 1..sample_entries().len() {
            let mut permuted = sample_entries();
            permuted.rotate_left(rotate);
            let other = rules_from_entries(permuted);
            assert_eq!(
                base.city_alias, other.city_alias,
                "city_alias order leaked (rotate {rotate})"
            );
            assert_eq!(
                base.abbrev2, other.abbrev2,
                "abbrev2 order leaked (rotate {rotate})"
            );
            assert_eq!(
                base.capitals, other.capitals,
                "capitals order leaked (rotate {rotate})"
            );
            assert_eq!(
                base.types_cyr, other.types_cyr,
                "types_cyr order leaked (rotate {rotate})"
            );
            assert_eq!(
                base.types_latin, other.types_latin,
                "types_latin order leaked (rotate {rotate})"
            );
        }
        let mut reversed = sample_entries();
        reversed.reverse();
        let other = rules_from_entries(reversed);
        assert_eq!(base.city_alias, other.city_alias);
        assert_eq!(base.abbrev2, other.abbrev2);
    }

    /// A truncated or corrupt section must never install a partial rule set.
    #[test]
    fn corrupt_section_installs_nothing() {
        let good = serialize_entries(&sample_entries());
        for cut in [1usize, 5, 9, good.len() - 1] {
            let truncated = &good[..cut.min(good.len())];
            let parsed = parse_entries(truncated);
            assert!(
                parsed.len() < sample_entries().len(),
                "truncation at {cut} parsed too much"
            );
        }
    }
}
