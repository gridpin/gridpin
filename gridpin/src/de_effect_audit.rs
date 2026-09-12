//! Ignored, test-only trace for Germany F3 rule counters.
//!
//! This module is deliberately absent from release builds.  It invokes the
//! real private DE transformations over the pinned fixtures before `de.bin`
//! exists.  It can prove that a retry/observer was emitted and show the changed
//! text; it cannot claim that the retry won or improved a coordinate result.

use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const EFFECTS: &[&str] = &[
    "orthography",
    "street_type",
    "city_alias",
    "frankfurt_qualifier",
    "abbreviation",
    "admin_tail",
    "recipient_prefix",
    "subaddress_tail",
    "parenthetical_subaddress",
    "address_field",
    "locality_first",
    "missing_comma_postcode_boundary",
    "country_token",
    "postcode_prefix",
    "house_suffix",
    "house_range",
    "house_slash",
    "postcode_zero",
    "postcode_seam",
    "postcode_five_digits",
];

#[derive(Clone)]
struct Input {
    id: String,
    query: String,
    live_base: String,
    source: String,
}

#[derive(Serialize)]
struct Example {
    id: String,
    query: String,
    live_base: String,
    source: String,
    output: String,
    combined_effects: Vec<String>,
    event_kind: &'static str,
}

#[derive(Default, Serialize)]
struct EffectStats {
    inputs_touched: usize,
    emitted_events: usize,
    examples: Vec<Example>,
}

#[derive(Serialize)]
struct CorpusReport {
    corpus: &'static str,
    rows: usize,
    counters: BTreeMap<String, EffectStats>,
}

#[derive(Serialize)]
struct Report {
    schema: u8,
    kind: &'static str,
    semantics: BTreeMap<&'static str, &'static str>,
    corpora: Vec<CorpusReport>,
}

struct Event {
    effect: &'static str,
    output: String,
    combined_effects: Vec<String>,
    event_kind: &'static str,
}

fn effect_key(effect: crate::de::Effect) -> &'static str {
    use crate::de::Effect;
    match effect {
        Effect::Orthography => "orthography",
        Effect::CityAlias => "city_alias",
        Effect::OfficialCommuneAlias => "official_commune_alias",
        Effect::Abbreviation => "abbreviation",
        Effect::AdminTail => "admin_tail",
        Effect::RecipientPrefix => "recipient_prefix",
        Effect::SubaddressTail => "subaddress_tail",
        Effect::ParentheticalSubaddress => "parenthetical_subaddress",
        Effect::AddressField => "address_field",
        Effect::LocalityFirst => "locality_first",
        Effect::MissingCommaPostcodeBoundary => "missing_comma_postcode_boundary",
        Effect::Country => "country_token",
        Effect::HouseRange => "house_range",
        Effect::HouseSlash => "house_slash",
        Effect::PostcodePrefix => "postcode_prefix",
        Effect::PostcodeZero => "postcode_zero",
    }
}

fn trace(query: &str, parser_index: &crate::query::Index) -> Vec<Event> {
    let mut events = Vec::new();
    for variant in crate::de::query_variants(query) {
        if variant.effects.is_empty() {
            continue;
        }
        let combined_effects: Vec<String> = variant
            .effects
            .iter()
            .copied()
            .map(effect_key)
            .map(str::to_string)
            .collect();
        for effect in &variant.effects {
            events.push(Event {
                effect: effect_key(*effect),
                output: variant.query.clone(),
                combined_effects: combined_effects.clone(),
                event_kind: "generated_query_variant",
            });
        }
    }

    // Observe the real free-form parser call sites, not a whole-query proxy.
    // The one-row live-coordinate DE index is only a driver: these are distinct
    // street-key attempts emitted by `Index::query`, not selected hits.
    crate::de::begin_street_runtime_trace();
    let _ = parser_index.query(query, 1);
    let street_attempts: BTreeSet<(String, String)> =
        crate::de::take_street_runtime_trace().into_iter().collect();
    for (phrase, output) in street_attempts {
        events.push(Event {
            effect: "street_type",
            output: format!("{phrase} -> {output}"),
            combined_effects: vec!["street_type".to_string()],
            event_kind: "runtime_parser_street_key_attempt_lab_index",
        });
    }
    if let Some(commune) = crate::de::frankfurt_qualifier(query) {
        events.push(Event {
            effect: "frankfurt_qualifier",
            output: format!("hard commune constraint: {commune}"),
            combined_effects: vec!["frankfurt_qualifier".to_string()],
            event_kind: "hard_postcondition",
        });
    }
    if let Some(output) = crate::query::de_house_suffix_trace(query) {
        events.push(Event {
            effect: "house_suffix",
            output,
            combined_effects: vec!["house_suffix".to_string()],
            event_kind: "parser_syntax_observer",
        });
    }
    if let Some(output) = crate::norm::de_postcode_seam_trace(query) {
        events.push(Event {
            effect: "postcode_seam",
            output,
            combined_effects: vec!["postcode_seam".to_string()],
            event_kind: "normalizer_observer",
        });
    }
    if let Some(output) = crate::query::de_five_digit_postcode_trace(query) {
        events.push(Event {
            effect: "postcode_five_digits",
            output,
            combined_effects: vec!["postcode_five_digits".to_string()],
            event_kind: "parser_invariant_observer",
        });
    }
    events
}

fn read_inputs(
    path: &Path,
    delimiter: u8,
    query_column: &str,
    base_column: Option<&str>,
    source_column: Option<&str>,
) -> Vec<Input> {
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .from_path(path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    let headers = reader.headers().expect("fixture headers").clone();
    let column = |name: &str| {
        headers
            .iter()
            .position(|header| header == name)
            .unwrap_or_else(|| panic!("{} lacks column {name}", path.display()))
    };
    let query_index = column(query_column);
    let base_index = base_column.map(column);
    let source_index = source_column.map(column);
    let line_index = headers.iter().position(|header| header == "line");
    reader
        .records()
        .enumerate()
        .map(|(offset, row)| {
            let row = row.expect("valid pinned CSV row");
            let query = row.get(query_index).unwrap_or("").to_string();
            let live_base = base_index
                .and_then(|index| row.get(index))
                .unwrap_or(&query)
                .to_string();
            let source = source_index
                .and_then(|index| row.get(index))
                .map(str::to_string)
                .or_else(|| {
                    line_index
                        .and_then(|index| row.get(index))
                        .map(|line| format!("real_de.csv:line={line}"))
                })
                .unwrap_or_else(|| format!("{}:row={}", path.display(), offset + 2));
            Input {
                id: line_index
                    .and_then(|index| row.get(index))
                    .map(str::to_string)
                    .unwrap_or_else(|| (offset + 1).to_string()),
                query,
                live_base,
                source,
            }
        })
        .collect()
}

fn summarize(
    corpus: &'static str,
    inputs: Vec<Input>,
    parser_index: &crate::query::Index,
) -> CorpusReport {
    let rows = inputs.len();
    let mut counters: BTreeMap<String, EffectStats> = EFFECTS
        .iter()
        .map(|effect| ((*effect).to_string(), EffectStats::default()))
        .collect();
    for input in inputs {
        let mut touched = BTreeSet::new();
        for event in trace(&input.query, parser_index) {
            let stats = counters
                .get_mut(event.effect)
                .unwrap_or_else(|| panic!("uncatalogued trace effect {}", event.effect));
            stats.emitted_events += 1;
            if touched.insert(event.effect) {
                stats.inputs_touched += 1;
                if stats.examples.len() < 2 {
                    stats.examples.push(Example {
                        id: input.id.clone(),
                        query: input.query.clone(),
                        live_base: input.live_base.clone(),
                        source: input.source.clone(),
                        output: event.output,
                        combined_effects: event.combined_effects,
                        event_kind: event.event_kind,
                    });
                }
            }
        }
    }
    CorpusReport {
        corpus,
        rows,
        counters,
    }
}

fn build_parser_trace_index(rules_dir: &Path) -> crate::query::Index {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "gridpin-de-f3-runtime-trace-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create trace index directory");
    let csv = dir.join("addresses.csv");
    std::fs::write(
        &csv,
        concat!(
            "nom_voie_norm,code_insee,nom_commune_norm,code_postal,code_postal_display,numero,rep,lon,lat,nom_voie,nom_commune\n",
            "cacilienstraße,KO01,koln,50676,50676,29,33,6.95143889,50.93471389,Cäcilienstraße,Köln\n"
        ),
    )
    .expect("write live Q950 trace row");
    let manifest = dir.join("manifest.json");
    std::fs::write(
        &manifest,
        r#"{"country":"de","layer":"addresses","license":"test-only live Q950 witness","source_release":"pinned F3"}"#,
    )
    .expect("write trace manifest");
    let bin = dir.join("addresses.bin");
    crate::builder::build(
        &csv,
        &bin,
        None,
        None,
        Some(rules_dir),
        None,
        Some(&manifest),
    )
    .expect("build one-row DE parser trace index");
    crate::query::Index::open(&bin).expect("open one-row DE parser trace index")
}

fn fixture_path(env_name: &str, default: &str) -> PathBuf {
    std::env::var_os(env_name).map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(default),
        PathBuf::from,
    )
}

#[test]
#[ignore = "explicit F3 trace: verifies B and traces the frozen 300+75 inputs"]
fn frozen_effect_trace() {
    let rules_dir = fixture_path("GRIDPIN_DE_TRACE_RULES", "../rules");
    let entries = crate::rules::entries_from_tsv_dir(&rules_dir).expect("read rule tables");
    let section = crate::rules::serialize_entries(&entries);
    let installed = crate::rules::from_section(&section);
    let _scope = crate::rules::scope(installed);
    let parser_index = build_parser_trace_index(&rules_dir);

    let real = read_inputs(
        &fixture_path("GRIDPIN_DE_TRACE_REAL", "../eval/scrape/real_de.csv"),
        b',',
        "address",
        None,
        Some("source"),
    );
    let sample = read_inputs(
        &fixture_path(
            "GRIDPIN_DE_TRACE_SAMPLE",
            "../eval/work/de_bench_20260820/de_bench2_sample.csv",
        ),
        b',',
        "address",
        None,
        None,
    );
    let adversarial = read_inputs(
        &fixture_path("GRIDPIN_DE_TRACE_ADVERSARIAL", "../eval/adversarial/de.csv"),
        b';',
        "query",
        Some("base_address"),
        Some("source"),
    );
    assert_eq!(real.len(), 5_000, "accepted B row count drift");
    assert_eq!(sample.len(), 300, "frozen sample row count drift");
    assert_eq!(adversarial.len(), 75, "frozen C row count drift");

    let report = Report {
        schema: 2,
        kind: "de_f3_prebuild_rust_effect_trace",
        semantics: BTreeMap::from([
            (
                "inputs_touched",
                "unique inputs for which the real Rust transformation/observer emitted an event",
            ),
            (
                "emitted_events",
                "all generated retries/observer events; one input may emit several",
            ),
            (
                "claim_boundary",
                "pre-build parser evidence only: not a selected hit, pass, improvement, or coordinate claim",
            ),
        ]),
        corpora: vec![
            summarize("frozen_sample_300", sample, &parser_index),
            summarize("adversarial_c_75", adversarial, &parser_index),
        ],
    };
    let output = std::env::var_os("GRIDPIN_DE_TRACE_OUT")
        .map(PathBuf::from)
        .expect("GRIDPIN_DE_TRACE_OUT is required for the ignored trace");
    std::fs::write(
        &output,
        serde_json::to_vec_pretty(&report).expect("serialize trace"),
    )
    .unwrap_or_else(|error| panic!("cannot write {}: {error}", output.display()));
    eprintln!("DE_F3_TRACE={}", output.display());
}
