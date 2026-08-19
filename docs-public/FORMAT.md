# GridPin Index File Format

This document describes the on-disk format of a GridPin index file
(`*.bin`), as implemented in `gridpin/src/index.rs`, `gridpin/src/builder.rs`,
`gridpin/src/rules.rs`, `gridpin/src/query.rs`, and `gridpin/src/ml.rs`. The code is the
authoritative source; where this document and the code disagree, the
code wins.

**Format identification:** magic `GPC0`, version byte `7`.

---

## 1. Overview

A GridPin index is a **single file designed to be memory-mapped** and
read in place. The engine opens the file with `mmap`, parses a small
fixed-size header, and from then on reads sections lazily — nothing is
copied or decompressed up front, so opening a multi-gigabyte index is
effectively instant and memory usage is driven by the OS page cache.

The file consists of:

1. A fixed-size **header** containing a magic number, a version byte,
   and a **section table** (id, offset, length for each section).
2. The **section payloads**, written back to back immediately after the
   header, in ascending section-id order.

Readers MUST locate sections through the section table, not by assuming
any particular layout of the payload area.

General conventions:

* **Byte order:** all multi-byte integers are **little-endian**.
* **Coordinates** are stored as fixed-point `i32` values in units of
  10⁻⁷ degrees (i.e. `round(degrees × 1e7)`).
* **varint** — unsigned LEB128-style encoding: 7 payload bits per byte,
  least-significant group first; the high bit of each byte is set when
  more bytes follow (`gridpin/src/index.rs`, `write_varint`/`read_varint`).
* **zigzag** — signed-to-unsigned mapping `(v << 1) ^ (v >> 63)` applied
  before varint encoding, so small negative deltas stay small
  (`gridpin/src/index.rs`, `zigzag`/`unzigzag`).
* **FST sections** contain a serialized `fst::Map` from the Rust
  [`fst`](https://crates.io/crates/fst) crate (a finite-state transducer
  mapping byte-string keys to `u64` values). Their internal byte layout
  is defined by that crate and is not re-specified here.
* **Strings** are UTF-8.

Terminology: a *commune* is the smallest administrative locality
(municipality / populated place); the name comes from the French
seed dataset but the structure is country-agnostic. A *street* record
is one (normalized street name, commune) pair. A *house* is one
addressed point on a street.

---

## 2. Header layout

Defined in `gridpin/src/index.rs` (`header_size`, `write_header`,
`parse_sections`).

| Offset | Size | Field | Description |
|-------:|-----:|-------|-------------|
| 0 | 4 | magic | ASCII `GPC0` (`0x47 0x50 0x43 0x30`) |
| 4 | 1 | version | Format version. Current value: **7** |
| 5 | 1 | nsec | Number of section-table entries that follow |
| 6 | nsec × 17 | section table | `nsec` entries, 17 bytes each (below) |

Each section-table entry:

| Offset in entry | Size | Field | Description |
|-------:|-----:|-------|-------------|
| 0 | 1 | id | Section id, `u8` (valid ids: 1…17; readers reject unknown ids) |
| 1 | 8 | off | `u64` LE — absolute byte offset of the section payload from the start of the file |
| 9 | 8 | len | `u64` LE — payload length in bytes; **0 means the section is absent** |

The current builder always writes all 17 entries (`nsec = 17`), in
ascending id order, with contiguous ascending offsets starting right
after the header; the total header size is therefore fixed at
`4 + 1 + 1 + 17 × 17 = 295` bytes. Readers, however, only rely on the
table itself: `parse_sections` reads `nsec` entries and rejects duplicate,
overlapping, missing-required, or unknown section ids.
The table may legally contain fewer than 17 entries (`gridpin repack`,
for instance, writes only the entries the source sheet had plus
`meta`); section ids absent from the table are treated as absent
(`len = 0`). Regardless of `nsec`, the file must be at least 295 bytes
— readers size the header by the full 17-entry table.

Validation performed by `parse_sections`:

* file shorter than the full-table header (295 bytes), or magic ≠
  `GPC0` → error ("not a GPC0 index file");
* version byte outside the supported read range (currently 6…7) → error
  naming the supported range and requiring a rebuild (see §7);
* `nsec` > 17 → error ("file is corrupt");
* a table entry whose `off + len` overflows or exceeds the file size →
  error (truncated download or corrupt file).

---

## 3. Section directory

Seventeen section ids are defined (`gridpin/src/index.rs`). Section id 0 is never
used. "Required" below means the engine cannot answer queries without
it; optional sections may legally have `len = 0` in the table.

| Id | Name | Contents | Required |
|---:|------|----------|:--------:|
| 1 | `communes_fst` | FST: normalized commune name → packed `(start << 16) \| count` reference into `commune_postings` | yes |
| 2 | `communes_meta` | 16-byte record per commune: identifier, display-name offset, significance (§4.2) | yes |
| 3 | `commune_postings` | Flat `u32` array of commune ids; groups of same-named communes (§4.3) | yes |
| 4 | `streets_fst` | FST: `normalized street name` + `0x1F` + `commune code` → `street_id` (§4.4) | yes |
| 5 | `streets_meta` | 36-byte record per street (§5) | yes |
| 6 | `house_blocks` | Per-street varint-compressed house lists: number deltas, suffix id, coordinate deltas, and sparse per-house postcodes where needed (§4.6) | yes |
| 7 | `names` | Display-string blob: `u8` length + UTF-8 bytes per entry (§4.7) | yes |
| 8 | `reps` | Dictionary of house-number suffixes ("bis", "ter", …): `u32` count, then `u8` length + bytes each (§4.8) | yes |
| 9 | `cells` | Reverse-geocoding grid (0.01° cells): directory of `(cell, start, count)` triples + `u32` street-id postings (§4.9) | yes |
| 10 | `parser` | Trained address-parsing model, magic `GPML` (§4.10) | no |
| 11 | `rank` | Trained ranking weights, magic `GPRK` (§4.11) | no |
| 12 | `words` | FST: street-name word (≥ 3 chars) or `~`-prefixed phonetic key → `u64` offset into `word_postings` (§4.12) | yes |
| 13 | `word_postings` | Per-word posting list: varint count + delta-varint sorted street ids (§4.12) | yes |
| 14 | `commune_coords` | 8 bytes per commune: address-weighted centroid `lat_c i32`, `lon_c i32` (§4.13) | yes |
| 15 | `rules` | Rules-in-data: curated alias/type-word/stop-word tables that travel with the data (§6) | no |
| 16 | `mark` | Optional per-copy buyer watermark: `GPMK`, layer version, build timestamp, mark string (§4.14). Paid copies carry a mark identifying the licensee. Its construction and the verification tooling are not published; both live only in the private distribution builds of the CLI. A sheet without a mark is byte-identical to one built with the public engine | no |
| 17 | `meta` | Provenance + identity record: `u32` pair count, then sorted UTF-8 key/value pairs (§4.15). Written from the build manifest (`--meta`) | no |

"Required" is ENFORCED at open: the reader rejects a sheet where any of
sections 1–9, 12 or 14 is absent or has `len = 0` (an empty required
section is treated as absent, and answering from it would silently
degrade results). Section 13 (`word_postings`) is the one exception:
its TOC entry must be PRESENT, but its payload may legally be empty —
and only when the `words` FST (section 12) has no keys; a non-empty
`words` FST with an empty postings section is rejected as corrupt, and
a non-empty payload is content-validated per word (see §4.12).
Sections 10, 11, 15 and 17 are optional (absent or malformed →
fallback, see §7); 16 is not read by the engine at all (only by the
private verification tooling).

---

## 4. Section payloads

### 4.1 `communes_fst` (id 1)

An `fst::Map`. Key: the normalized commune name (also "umbrella city"
alias names, see below). Value (`u64`): `(start << 16) | count`, where

* `count` (low 16 bits) — number of communes bearing this name;
* `start` — index (in `u32` units, i.e. byte offset ÷ 4) of the first
  entry of this name-group inside `commune_postings`.

Empty names are skipped at build time. A commune may appear under
several keys: its own normalized name plus optional umbrella-city names
supplied by the build input (`provincia_norm` column, `|`-separated),
so e.g. "Beograd" also resolves to its constituent populated places.

### 4.2 `communes_meta` (id 2)

Array of fixed 16-byte records (`COMMUNE_META_SIZE = 16`); the record
index is the `commune_id`, assigned sequentially from 0 in order of
first appearance in the build input.

| Offset | Size | Field | Description |
|-------:|-----:|-------|-------------|
| 0 | 8 | code | Commune identifier (INSEE-style code) as raw bytes, zero-padded to 8; codes longer than 8 bytes are truncated |
| 8 | 4 | name_off | `u32` — offset of the display name in `names` |
| 12 | 4 | significance | `u32` — total number of houses (addressed points) in the commune; a population proxy used for ranking tie-breaks and capital-anchor selection |

Note: a doc comment in `gridpin/src/index.rs` describes the first field as
`insee[5] + pad[3]`; the builder actually writes a generic 8-byte
zero-padded field (French INSEE codes happen to be 5 bytes + 3 zero
bytes). The `significance` word is back-patched after the main build
pass (`gridpin/src/builder.rs`).

### 4.3 `commune_postings` (id 3)

A flat array of `u32` (LE) commune ids. `communes_fst` values point
into it (§4.1); each name-group occupies `count` consecutive entries.
Exists because commune names are not unique.

### 4.4 `streets_fst` (id 4)

An `fst::Map`. Key: the normalized street name, the separator byte
`0x1F` (`KEY_SEP`), then the commune code (same string that fills the
`code` field of §4.2, un-padded). Value (`u64`): `street_id`.

Street ids are assigned sequentially from 0 in key order (the build
input is pre-sorted so that street emission order coincides with the
lexicographic key order required by the FST builder).

### 4.5 `streets_meta` (id 5)

Array of fixed 36-byte records indexed by `street_id`; see §5 for the
record layout.

### 4.6 `house_blocks` (id 6)

Concatenated per-street house blocks. A street's block starts at the
byte offset given by its `house_off` field (§5) and contains exactly
`house_count` house entries.

Most streets use the compact legacy grammar: every entry is four
varints in order:

| # | Encoding | Meaning |
|--:|----------|---------|
| 1 | varint | House number: **absolute** value for the first house of the block, **delta from the previous house's number** for the rest. Numbers are non-decreasing within a block (guaranteed by the sorted build input) |
| 2 | varint | `rep_id` — house-number suffix: 0 = no suffix, otherwise 1-based index into the `reps` dictionary (§4.8) |
| 3 | zigzag varint | `lat − lat_c` — latitude delta from the street centroid, in 10⁻⁷ degree units |
| 4 | zigzag varint | `lon − lon_c` — longitude delta from the street centroid, in 10⁻⁷ degree units |

Version 7 adds a conditional, sparse house-postcode grammar. It is used
only when the corresponding street metadata has
`pc_disp_off = 0xFFFFFFFF` (`PC_DISP_AMBIGUOUS`): that means the houses
have more than one *effective* postcode value, counting a missing value
when it is mixed with a known postcode. The block then begins with:

1. `varint postcode_count` — number of distinct **non-empty** local
   postcode strings;
2. exactly `postcode_count` little-endian `u32` offsets into `names`,
   in lexicographically sorted postcode order;
3. `house_count` entries, each with the four varints above followed by
   a fifth `varint postcode_id`: 0 = missing, 1…`postcode_count` =
   1-based index in the local dictionary.

All-missing streets and streets with one effective known value keep the
four-varint grammar and have no dictionary prefix. Thus the extra cost
is paid only where a street-level postcode would be inaccurate. Readers
must inspect `pc_disp_off` before decoding the block; there is no
self-describing tag on ordinary four-varint blocks.

### 4.7 `names` (id 7)

A blob of length-prefixed display strings: each entry is a `u8` byte
length (0…255) followed by that many bytes of UTF-8. A string longer
than 255 bytes is **rejected at build time** (the build fails loudly)
rather than silently truncated to a different value. Other sections
refer to entries by the byte offset of the length prefix.

Offset 0 is always occupied by the first commune's display name; this
is what allows other fields (e.g. `pc_disp_off`, §5) to use 0 as an
"absent" sentinel.

### 4.8 `reps` (id 8)

Dictionary of house-number suffixes ("bis", "ter", "A", …):

| Offset | Size | Field |
|-------:|-----:|-------|
| 0 | 4 | `u32` count |
| 4… | | count entries, each: `u8` length + UTF-8 bytes (max 255 bytes) |

Entry *i* (in file order, starting at the first entry) has
`rep_id = i` counted from 1; `rep_id = 0` means "no suffix" and has no
entry in this section.

### 4.9 `cells` (id 9)

Reverse-geocoding grid on 0.01° cells (≈ 1.1 km). The cell number of a
coordinate is (`cell_of` in `gridpin/src/index.rs`):

```
lat_idx = clamp(floor((lat +  90) / 0.01), 0, 17999)
lon_idx = clamp(floor((lon + 180) / 0.01), 0, 35999)
cell    = lat_idx * 36000 + lon_idx        (u32)
```

Section layout:

| Offset | Size | Field |
|-------:|-----:|-------|
| 0 | 4 | `u32 n_dir` — number of directory entries |
| 4 | n_dir × 12 | directory: `(cell u32, start u32, count u32)` per non-empty cell, sorted by `cell` ascending |
| 4 + n_dir × 12 | … | postings: flat `u32` array of street ids |

`start` is an index in `u32` units into the postings area (i.e. the
posting's byte offset is `4 + n_dir × 12 + start × 4`). Each street
appears in exactly one cell — the one containing its centroid.

Because a street lives in a single centroid cell and reverse scans a bounded
window (~10 km) around the query, **reverse geocoding is an _approximate_
nearest, not a guaranteed global nearest**: a closer house on a street whose
centroid falls in a farther cell can be missed. Every reverse hit carries
`distance_m` so the caller sees how far the answer actually is.

**Admin sidecar.** Reverse regions live in an OPTIONAL sibling file whose name
the engine derives from the sheet as `<sheet-stem>_admin.bin` (e.g. `fr.bin` →
`fr_admin.bin`). It MUST be renamed/copied alongside the sheet on every release
rename (`fr.bin` → `fr-2026.07.gpin` needs `fr-2026.07_admin.bin`), or regions
silently disappear; the loader warns when it finds a mismatched sibling.

### 4.10 `parser` (id 10) — optional

A trained multinomial logistic-regression model for address-token
classification, embedded verbatim by the builder from an external file
and decoded by `Parser::from_section` (`gridpin/src/ml.rs`):

| Offset | Size | Field |
|-------:|-----:|-------|
| 0 | 4 | magic `GPML` |
| 4 | 4 | `u32 dim` — feature-hash dimension |
| 8 | 1 | `u8 classes` — number of output classes |
| 9 | classes × 4 | `f32` intercepts, one per class |
| 9 + classes × 4 | classes × dim × 4 | `f32` coefficient matrix, row-major by class |

If this section is PRESENT, the reader validates it strictly: a wrong
magic or a coefficient area that is not exactly `classes × dim × 4`
bytes is a corrupt/tampered sheet and the open **fails** (see §7). Only
an ABSENT section falls back to the built-in heuristic.

### 4.11 `rank` (id 11) — optional

Trained linear ranking weights, embedded verbatim and decoded by
`Rank::from_section` (`gridpin/src/query.rs`):

| Offset | Size | Field |
|-------:|-----:|-------|
| 0 | 4 | magic `GPRK` |
| 4 | 1 | `u8 n` — number of weights |
| 5 | 4 | `f32 bias` |
| 9 | n × 4 | `f32` weights |

### 4.12 `words` (id 12) and `word_postings` (id 13)

Inverted index over street-name words, enabling search by a subset of
words and fuzzy/phonetic matching.

`words` is an `fst::Map`. Keys are of two kinds:

* a normalized street-name word of ≥ 3 characters that is not merely a
  street-type word;
* the word's phonetic key prefixed with `~` (byte `0x7E`), emitted only
  when the phonetic key differs from the word itself and is ≥ 3
  characters (`phonetic_key` in `gridpin/src/norm.rs`).

The FST value (`u64`) is a byte offset into `word_postings`, where the
word's posting list is encoded as:

| # | Encoding | Meaning |
|--:|----------|---------|
| 1 | varint | count of street ids |
| 2… | varint × count | street ids, sorted ascending, delta-encoded (first value is the delta from 0, i.e. absolute) |

Posting lists are deduplicated and capped at **16 384** street ids per
word at build time (overly common words are truncated).

Both sections must have a TOC entry. `word_postings` may be **empty
only when the `words` FST has no keys** (every street word is shorter
than 3 characters); a non-empty `words` FST with an empty postings
section is corrupt. A reader validates the content on open: every FST
key must decode to a list with a count of 1…16 384, complete varints
inside the section, strictly increasing street ids, each below the
street count — a present-but-zeroed or truncated payload is rejected
rather than silently disabling fuzzy search.

### 4.13 `commune_coords` (id 14)

Array of fixed 8-byte records indexed by `commune_id`:

| Offset | Size | Field |
|-------:|-----:|-------|
| 0 | 4 | `i32 lat_c` — 10⁻⁷ degrees |
| 4 | 4 | `i32 lon_c` — 10⁻⁷ degrees |

The address-count-weighted centroid of the commune's street centroids;
used as the "city point" for city-only queries and as the capital
anchor.

### 4.14 `rules` (id 15) — optional

See §6.

### 4.15 `meta` (id 17) — optional

Provenance + identity record, new in v6 (`encode_meta` /
`decode_meta` in `gridpin/src/index.rs`):

| Offset | Size | Field |
|-------:|-----:|-------|
| 0 | 4 | `u32 n` — number of key/value pairs (readers reject n > 1024) |
| 4… | | n pairs, back to back |

Each pair:

| # | Size | Field |
|--:|-----:|-------|
| 1 | 2 | `u16 klen` — key length in bytes |
| 2 | klen | key, UTF-8 |
| 3 | 4 | `u32 vlen` — value length in bytes |
| 4 | vlen | value, UTF-8 |

The writer sorts pairs by key, so identical inputs produce identical
bytes. The section is filled from a JSON build manifest (`gridpin
build … --meta manifest.json`; `meta_from_manifest` in
`gridpin/src/builder.rs`): scalar manifest values become strings,
arrays/objects are embedded as compact JSON. Standard keys:

| Key | Contents |
|-----|----------|
| `country` | Lowercase ISO country code (lowercased by the writer; required in the manifest) |
| `layer` | `addresses` or `poi`; any other value is rejected at build time (required in the manifest) |
| `license` | License of the source dataset |
| `sources` | Human-readable source dataset name(s) |
| `source_release` | Release/extract date of the source data |
| `attribution` | Attribution line required by the source license |

The writer also STAMPS these keys itself (they are not taken from the
manifest, and any manifest copy of them is overwritten), so a sheet's
provenance identifies exactly how it was produced:

| Key | Stamped value |
|-----|---------------|
| `meta_schema` | SEC_META schema version (currently `1`) |
| `builder_version` | The `gridpin` crate version that built the sheet |
| `builder_target` | Build host `os-arch` (e.g. `linux-x86_64`) |
| `builder_git` | Git commit that built the tool (`git rev-parse`, `-dirty` suffix if the tree was modified; `unknown` outside a checkout) |
| `input_blake2b256` | Hex BLAKE2b-256 of the input file bytes as-is (the source artifact identity) |

Reader semantics:

* A malformed section (truncated, invalid UTF-8, n > 1024) decodes to
  **"no provenance"** (`decode_meta` returns `None`); it MUST NOT be
  treated as an open error or crash the reader.
* `country` and `layer` are the sheet's **identity**, checked at load
  time when an address sheet and a POI layer are opened as a pair
  (`check_pair` in `gridpin/src/query.rs`): differing `country` values, a base
  sheet whose `layer` ≠ `addresses`, or a `--poi` file whose `layer` ≠
  `poi` are refused. A sheet without the section (or without these
  keys) only produces a warning — the pair is not verified.
* `gridpin meta <file>` prints the record.

---

## 5. Street meta record (36 bytes)

`streets_meta` (§4.5) is an array of fixed-size records,
`STREET_META_SIZE = 36` bytes, indexed by `street_id`. Field order as
written by `gridpin/src/builder.rs`:

| Offset | Size | Type | Field | Description |
|-------:|-----:|------|-------|-------------|
| 0 | 4 | `i32` | lat_c | Street centroid latitude, 10⁻⁷ degrees (mean of house latitudes) |
| 4 | 4 | `i32` | lon_c | Street centroid longitude, 10⁻⁷ degrees |
| 8 | 4 | `u32` | commune_id | Index into `communes_meta` / `commune_coords` |
| 12 | 4 | `u32` | postcode | Numeric postcode prefix used for ranking (`75002` → 75002; NL `1012XJ` → 1012); 0 = unknown |
| 16 | 4 | `u32` | name_off | Offset of the display street name in `names` |
| 20 | 8 | `u64` | house_off | Byte offset of this street's block in `house_blocks` |
| 28 | 4 | `u32` | house_count | Number of houses in the block |
| 32 | 4 | `u32` | pc_disp_off | Offset in `names` of the full display postcode string (e.g. NL `1012XJ`); **0 = absent**; **0xFFFFFFFF = multiple effective house-postcode values**, selecting the sparse v7 house-block grammar (§4.6) and requiring empty postcode for street-level results |

The numeric postcode is derived from the most frequent non-empty
postcode among the street's houses. An unambiguous display postcode is
stored directly. When the houses have multiple effective values
(including missing-vs-known), `pc_disp_off` is `0xFFFFFFFF`: exact and
reverse house results may use the per-house dictionary, while
street/city results stay empty rather than borrowing a neighbour's
postcode. The `0` sentinel is safe because offset 0 in `names` always
holds a commune name (§4.7).

---

## 6. `SEC_RULES` entry encoding (id 15)

The rules section carries curated tables (aliases, street-type words,
capitals, stop-lists) **inside the data file**, so that rule updates
ship as a data rebuild rather than an engine release. Encoding
(`serialize_entries` / `install_from_section` in `gridpin/src/rules.rs`):

| Offset | Size | Field |
|-------:|-----:|-------|
| 0 | 4 | `u32 n` — number of entries |
| 4… | | n entries, back to back |

Each entry:

| # | Size | Field |
|--:|-----:|-------|
| 1 | 1 | `u8 class` — table id (see below) |
| 2 | 2 | `u16 klen` — key length in bytes (max 65 535; longer keys are truncated by the writer) |
| 3 | klen | key, UTF-8 |
| 4 | 2 | `u16 vlen` — value length in bytes |
| 5 | vlen | value, UTF-8 |

Classes 1–3 are key→value tables; all other classes are word lists
where only the key is meaningful and the value SHOULD be empty
(readers ignore it). Class ids and names (from `CLASSES` in
`gridpin/src/rules.rs`; each name is also the base name of its source
`rules/<name>.tsv` file):

| Class | Name | Kind | Contents |
|------:|------|------|----------|
| 1 | `commune_alias` | key → value | Normalized commune-name alias → canonical name ("den haag" → "s gravenhage") |
| 2 | `city_alias` | key → value | Query-side city alias → canonical city ("milan" → "milano", "the hague" → "s gravenhage") |
| 3 | `abbrev2` | key → value | Two-word abbreviation, key = `"a b"` (two tokens joined by a space) → full word ("p zza" → "piazza") |
| 4 | `capitals` | list | Capital-city anchor names |
| 5 | `street_types_cyr` | list | Street-type words, Cyrillic script — e.g. Serbian "улица" (participate in type padding/rotation) |
| 6 | `affix_extra` | list | Additional affix words (residential-complex / district markers) |
| 7 | `noise` | list | Noise words stripped before a name (e.g. "grad") |
| 8 | `noise_after` | list | Noise words stripped after a name |
| 9 | `region_markers` | list | Region/district markers (e.g. "provincia", "okrug") |
| 10 | `countries_mid` | list | Country names recognized mid-query |
| 11 | `countries_tail` | list | Country names recognized at query tail |
| 12 | `place_junk` | list | Junk place-type words (settlement-type suffixes) |
| 13 | `place_prefix` | list | Strippable place prefixes (settlement/district type words) |
| 14 | `fr_ord_cities` | list | French cities with ordinal arrondissements ("paris", "lyon", "marseille") |
| 15 | `place_type_strip` | list | Place-type words strippable from place names |
| 16 | `street_types_latin` | list | Street-type words, Latin script (participate in type padding) |
| 17 | `street_types_extra` | list | Street-type words, recognition only ("straat", "ulica", …) |

Reader semantics (`install_from_section`):

* Entries with an **unknown class byte are silently ignored** — new
  classes can be added without breaking old engines.
* The section **supplements** the engine's built-in default tables
  (list entries are added; key→value entries of class 1 override
  defaults with the same key, since that table is a map).
* If the section is truncated or malformed at any point, installation
  is **aborted entirely** (no partial install) and the engine keeps its
  built-in defaults; a section shorter than 4 bytes is ignored the same
  way.
* Installation is **process-global and first-wins**: the first opened
  index that carries a non-empty rules section defines the rules for
  the whole process; later indexes' rules sections are silently
  ignored. All indexes built from the same rules catalog therefore
  carry identical tables by convention.

---

## 7. Versioning policy

The single version byte in the header (§2) acts as a **major format
version**; there are no minor versions. The builder writes version 7.
The current reader accepts v7 and, as a bounded migration feature, v6.
For v6 it uses the legacy four-varint house grammar and preserves the
old rule that a postcode-ambiguous street emits an empty postcode even
for a house result. Version 5 and versions newer than 7 are rejected at
open time with an explicit rebuild error (`parse_sections` in
`gridpin/src/index.rs`).

Version 7 changes the grammar of `house_blocks` for postcode-ambiguous
streets. Therefore v5 and v6 files **cannot** be upgraded by relabeling
or byte-copy repack; distributed v7 sheets must be rebuilt from the
sorted source CSV. Read compatibility for v6 does not weaken this rule.
`repack` accepts only an existing v7 sheet and is used to replace its
provenance record without touching data sections:

```
gridpin repack <in> <out> --meta manifest.json
```

which copies every data section byte-for-byte, replaces any existing
`meta` section from the manifest, and writes a fresh v7 header. Because
the data sections are untouched, the repacked file answers identically
to the source. `gridpin meta <file>` prints the record.

Two hard limits in `parse_sections` bound forward compatibility (§2):
a header declaring `nsec` > 17 is rejected as corrupt, and the file
must be at least 295 bytes (the full-table header size) even when
`nsec` < 17 — the table itself may legally have fewer entries, and
section ids absent from the table are treated as absent.

Within a version, an **absent** optional section degrades gracefully; a
**present-but-corrupt** one is treated as a damaged/tampered file:

* `parser` (10) and `rank` (11): if **absent** (zero length), the engine
  falls back to its built-in heuristic parsing and legacy hand-tuned
  ranking score. If **present but failing** its inner validation
  (`GPML`/`GPRK` magic, exact size, `rank` `n` must equal the fixed
  feature count, finite weights), the reader treats the sheet as corrupt
  and the open **fails** with an error rather than silently dropping the
  trained model — the build validates these sections too, so this fires
  only on a damaged or tampered file.
* `rules` (15): if absent or malformed, the engine runs on the built-in
  default tables compiled into `gridpin/src/rules.rs` — a "bare" engine and
  pre-v5 rebuilt files work without the section.
* `meta` (17): if absent or malformed, the engine carries no
  provenance/identity; the address/POI pairing check (§4.15) downgrades
  to a warning.

Unknown section ids in the table are rejected by the strict schema validator;
unknown rule classes inside `SEC_RULES` are skipped. Any change that alters the
meaning or layout of an existing section — or grows the section-id space — requires bumping
the version byte (as happened for v5, which introduced `SEC_RULES`,
v6, which introduced `SEC_META`, and v7, which introduced the conditional
house-postcode grammar in `SEC_HOUSE_BLOCKS`).

---

## 8. Stability

**This format is pre-release.** Until the project reaches v1.0, the
format may change in incompatible ways between any two builds, and the
version byte will be bumped without a deprecation period. Do not
archive index files as long-term artifacts and do not write third-party
readers against this document yet: rebuild indexes from source data
with the matching engine version instead. After v1.0, format changes
will follow the versioning policy in §7.
