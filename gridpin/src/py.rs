//! Python bindings (PyO3): `import gridpin; g = gridpin.Geocoder("data/france.bin")`.
//! POI cascade: `gridpin.Geocoder("france.bin", poi="fr_poi.bin")` — see query::query_cascade.

use std::path::PathBuf;

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use crate::query;

/// Single-country geocoder: opens an index file (mmap, effectively instant).
#[pyclass]
struct Geocoder {
    inner: query::Index,
    poi: Option<query::Index>,
}

/// The options of `houses` and `sample` as an area. `bbox` is (min_lon, min_lat, max_lon, max_lat), `near`
/// (lat, lon, metres), `polygon` the GeoJSON text.
fn area_of(
    street: Option<String>,
    city: Option<String>,
    postcode: Option<String>,
    near: Option<(f64, f64, f64)>,
    bbox: Option<(f64, f64, f64, f64)>,
    polygon: Option<String>,
    region: Option<String>,
) -> PyResult<query::Area> {
    let invalid = |e: String| pyo3::exceptions::PyValueError::new_err(e);
    let circle = near
        .map(|(lat, lon, metres)| query::parse_circle(&format!("{lat},{lon},{metres}")))
        .transpose()
        .map_err(invalid)?;
    let bbox = bbox
        .map(|(a, b, c, d)| query::BBox::parse(&format!("{a},{b},{c},{d}")))
        .transpose()
        .map_err(invalid)?;
    let polygons = polygon
        .as_deref()
        .map(query::PolygonSet::from_geojson)
        .transpose()
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
    Ok(query::Area {
        street,
        city,
        postcode,
        circle,
        bbox,
        polygons,
        region,
    })
}

/// One address of `geocode_fields_many`: street, number, city, region, postcode.
type FieldRow = (
    String,
    Option<String>,
    String,
    Option<String>,
    Option<String>,
);

fn hit_to_dict<'py>(py: Python<'py>, h: &query::Hit) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("lat", h.lat)?;
    d.set_item("lon", h.lon)?;
    d.set_item("precision", h.precision)?;
    d.set_item("score", h.score)?;
    d.set_item("street", &h.street)?;
    if let Some(hn) = &h.housenumber {
        d.set_item("housenumber", hn)?;
    }
    d.set_item("commune", &h.commune)?;
    d.set_item("postcode", &h.postcode)?;
    d.set_item("confidence", h.confidence)?;
    if !h.flags.is_empty() {
        d.set_item("flags", h.flags.clone())?;
    }
    // present only when the sheet ships administrative polygons — same response contract
    // as the CLI and the DuckDB extension
    if let Some(region) = &h.region {
        d.set_item("region", region)?;
    }
    // the engine's verdict that the answer needs a check before use, as in every JSON surface
    d.set_item("weak", query::hit_is_weak(h))?;
    let (level, status) = query::match_level(h);
    d.set_item("match_level", level)?;
    d.set_item("match_status", status.to_string())?;
    Ok(d)
}

/// A serializable result as plain Python objects, through the same JSON the CLI prints.
fn to_python<'py, T: serde::Serialize>(py: Python<'py>, value: &T) -> PyResult<Bound<'py, PyAny>> {
    let text = serde_json::to_string(value)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
    py.import("json")?.call_method1("loads", (text,))
}

/// `extras="h3,codes"` read against the sheet; a bad spec is a ValueError.
fn extras_of(spec: Option<&str>, idx: &query::Index) -> PyResult<Option<crate::extras::Extras>> {
    crate::extras::Extras::new(spec, idx)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))
}

/// The dict of one hit, with `extras` when asked for.
fn hit_dict<'py>(
    py: Python<'py>,
    h: &query::Hit,
    extras: Option<&crate::extras::Extras>,
    idx: &query::Index,
) -> PyResult<Bound<'py, PyDict>> {
    let d = hit_to_dict(py, h)?;
    if let Some(e) = extras {
        d.set_item("extras", to_python(py, &e.of(h, idx))?)?;
    }
    Ok(d)
}

#[pymethods]
impl Geocoder {
    #[new]
    #[pyo3(signature = (index_path, poi = None, *, also = None))]
    fn new(index_path: PathBuf, poi: Option<PathBuf>, also: Option<PathBuf>) -> PyResult<Self> {
        let mut inner = query::Index::open_address(&index_path)
            .map_err(|e| pyo3::exceptions::PyIOError::new_err(e.to_string()))?;
        if let Some(a) = also {
            // a sheet of one's own places (`gridpin build --places`), asked before the address sheet
            inner.set_also(
                query::Index::open_also(&a)
                    .map_err(|e| pyo3::exceptions::PyIOError::new_err(e.to_string()))?,
            );
        }
        let poi = match poi {
            Some(p) => Some(
                query::Index::open(&p)
                    .map_err(|e| pyo3::exceptions::PyIOError::new_err(e.to_string()))?,
            ),
            None => None,
        };
        if let Some(pi) = &poi {
            // v6 identity: a French POI layer over the Italian sheet must fail loudly
            query::check_pair(&inner, pi).map_err(pyo3::exceptions::PyValueError::new_err)?;
        }
        Ok(Geocoder { inner, poi })
    }

    /// Single query: returns a list of dicts (lat, lon, precision, score, street,
    /// optional housenumber, commune, postcode).
    #[pyo3(signature = (q, k = 10, *, extras = None))]
    fn geocode<'py>(
        &self,
        py: Python<'py>,
        q: &str,
        k: usize,
        extras: Option<&str>,
    ) -> PyResult<Bound<'py, PyList>> {
        let extras = extras_of(extras, &self.inner)?;
        let hits = py.detach(|| query::query_cascade(&self.inner, self.poi.as_ref(), q, k));
        let out = PyList::empty(py);
        crate::extras::for_query(q, || -> PyResult<()> {
            for h in &hits {
                out.append(hit_dict(py, h, extras.as_ref(), &self.inner)?)?;
            }
            Ok(())
        })?;
        Ok(out)
    }

    /// One query for an agent: dict with `kind` "answer" (and `results`), "clarify" (`results`, `missing` —
    /// "city" or "region" — and `options`, the places to choose from: label, city, region, lat, lon) or "none"
    /// (`reason`: no_street, no_house, city_unknown, outside_sheet, unparsed). `results` is what `geocode` gives.
    #[pyo3(signature = (q, k = 1))]
    fn geocode_or_ask<'py>(
        &self,
        py: Python<'py>,
        q: &str,
        k: usize,
    ) -> PyResult<Bound<'py, PyAny>> {
        use crate::outcome::Outcome;
        let outcome =
            py.detach(|| crate::outcome::query_outcome(&self.inner, self.poi.as_ref(), q, k));
        let out = to_python(py, &outcome)?;
        // the same dicts `geocode` gives, not their JSON round trip (confidence stays the engine's float)
        let results = match &outcome {
            Outcome::Answer { results } | Outcome::Clarify { results, .. } => Some(results),
            Outcome::None { .. } => None,
        };
        if let Some(results) = results {
            let list = PyList::empty(py);
            for h in results {
                list.append(hit_to_dict(py, h)?)?;
            }
            out.set_item("results", list)?;
        }
        Ok(out)
    }

    /// How the engine reads an address line: dict with `words`, `postcode` and `house_number` of the
    /// first parse hypothesis and `answer` (the top hit, or None). No new parsing rules: see `gridpin parse`.
    fn parse<'py>(&self, py: Python<'py>, q: &str) -> PyResult<Bound<'py, PyAny>> {
        let reading = py.detach(|| crate::compare::read(&self.inner, self.poi.as_ref(), q));
        to_python(py, &reading)
    }

    /// Addresses in free text, each confirmed by the sheet: list of dicts with `start`, `end` (byte offsets),
    /// `text`, `level` (house, street) and `hit`. With `verify`, also the addresses with a number the sheet does
    /// not have (`verdict` "not_found" and a `reason`). See `gridpin extract`.
    #[pyo3(signature = (text, *, verify = false))]
    fn extract<'py>(
        &self,
        py: Python<'py>,
        text: &str,
        verify: bool,
    ) -> PyResult<Bound<'py, PyAny>> {
        let (found, _) = py.detach(|| crate::extract::extract(&self.inner, text, verify));
        to_python(py, &found)
    }

    /// Does an address exist in the sheet? dict with `exists` (yes, maybe, no) and `reason`. See `gridpin exists`.
    fn exists<'py>(&self, py: Python<'py>, q: &str) -> PyResult<Bound<'py, PyAny>> {
        let e = py.detach(|| crate::exists::judge(&self.inner, q));
        to_python(py, &e)
    }

    /// Do two address lines name the same place? dict with `level` (house, street, commune, different,
    /// unknown), `reason`, `text` (only when a line has no answer) and both readings. "house" needs the
    /// same street, number and commune and no weak answer on either side.
    fn same_address<'py>(&self, py: Python<'py>, a: &str, b: &str) -> PyResult<Bound<'py, PyAny>> {
        let same = py.detach(|| crate::compare::same_address(&self.inner, self.poi.as_ref(), a, b));
        to_python(py, &same)
    }

    /// Batch of queries (rayon; defaults to cores minus two to keep the machine
    /// responsive; override with the GRIDPIN_THREADS env var — it is read when the
    /// pool for the current process is first built, so set it before the first call).
    /// Returns one list of up to `k` hits per query — the same shape as `geocode`,
    /// so `k` is honoured (an empty list means no match). Interruptible: Ctrl-C is
    /// checked between chunks and raises KeyboardInterrupt.
    #[pyo3(signature = (queries, k = 1, *, extras = None))]
    fn geocode_many<'py>(
        &self,
        py: Python<'py>,
        queries: Vec<String>,
        k: usize,
        extras: Option<&str>,
    ) -> PyResult<Bound<'py, PyList>> {
        use rayon::prelude::*;
        let extras = extras_of(extras, &self.inner)?;
        let pool = batch_pool();
        // Chunked execution: between chunks the GIL is re-acquired and pending
        // signals run, so Ctrl-C interrupts a long batch instead of piling up
        // until the very end. 1024 queries ≈ well under a second
        // even on the slow fuzzy path — the overhead is noise.
        const CHUNK: usize = 1024;
        let out = PyList::empty(py);
        for chunk in queries.chunks(CHUNK) {
            py.check_signals()?;
            let results: Vec<Vec<query::Hit>> = py.detach(|| {
                pool.install(|| {
                    chunk
                        .par_iter()
                        .map(|q| query::query_cascade(&self.inner, self.poi.as_ref(), q, k))
                        .collect()
                })
            });
            for (q, hits) in chunk.iter().zip(&results) {
                let row = PyList::empty(py);
                crate::extras::for_query(q, || -> PyResult<()> {
                    for h in hits {
                        row.append(hit_dict(py, h, extras.as_ref(), &self.inner)?)?;
                    }
                    Ok(())
                })?;
                out.append(row)?;
            }
        }
        Ok(out)
    }

    /// Addresses of an area as a list of dicts (lat, lon, street, housenumber, postcode, commune, region
    /// when known). The same options as `gridpin list`; with none, the whole sheet up to `limit`.
    #[pyo3(signature = (*, street = None, city = None, postcode = None, near = None, bbox = None, polygon = None, region = None, limit = None))]
    #[allow(clippy::too_many_arguments)]
    fn houses<'py>(
        &self,
        py: Python<'py>,
        street: Option<String>,
        city: Option<String>,
        postcode: Option<String>,
        near: Option<(f64, f64, f64)>,
        bbox: Option<(f64, f64, f64, f64)>,
        polygon: Option<String>,
        region: Option<String>,
        limit: Option<u64>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let area = area_of(street, city, postcode, near, bbox, polygon, region)?;
        let (filter, _) = self
            .inner
            .house_filter(&area)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
        let (rows, _) = py.detach(|| self.inner.houses(&filter, limit));
        to_python(py, &rows)
    }

    /// A uniform random sample of real addresses of an area; the same `seed` gives the same addresses.
    /// `seed` None takes one from the clock.
    #[pyo3(signature = (n, *, seed = None, street = None, city = None, postcode = None, near = None, bbox = None, polygon = None, region = None))]
    #[allow(clippy::too_many_arguments)]
    fn sample<'py>(
        &self,
        py: Python<'py>,
        n: usize,
        seed: Option<u64>,
        street: Option<String>,
        city: Option<String>,
        postcode: Option<String>,
        near: Option<(f64, f64, f64)>,
        bbox: Option<(f64, f64, f64, f64)>,
        polygon: Option<String>,
        region: Option<String>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let area = area_of(street, city, postcode, near, bbox, polygon, region)?;
        let (filter, _) = self
            .inner
            .house_filter(&area)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
        let seed = seed.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |t| t.as_nanos() as u64)
        });
        let (rows, _) = py.detach(|| self.inner.sample_houses(&filter, n, seed));
        to_python(py, &rows)
    }

    /// Batch of addresses already split into fields: one `(street, number, city, region, postcode)` tuple
    /// per address; `number`, `region` and `postcode` may be None and `city` empty. Same shape, pool and
    /// Ctrl-C as `geocode_many`.
    #[pyo3(signature = (rows, k = 1, *, extras = None))]
    fn geocode_fields_many<'py>(
        &self,
        py: Python<'py>,
        rows: Vec<FieldRow>,
        k: usize,
        extras: Option<&str>,
    ) -> PyResult<Bound<'py, PyList>> {
        use rayon::prelude::*;
        let extras = extras_of(extras, &self.inner)?;
        let pool = batch_pool();
        const CHUNK: usize = 1024;
        let out = PyList::empty(py);
        for chunk in rows.chunks(CHUNK) {
            py.check_signals()?;
            let results: Vec<Vec<query::Hit>> = py.detach(|| {
                pool.install(|| {
                    chunk
                        .par_iter()
                        .map(|(street, number, city, region, postcode)| {
                            query::query_fields_cascade(
                                &self.inner,
                                self.poi.as_ref(),
                                query::AddressFields {
                                    street,
                                    number: number.as_deref(),
                                    city,
                                    region: region.as_deref(),
                                    postcode: postcode.as_deref(),
                                },
                                k,
                            )
                        })
                        .collect()
                })
            });
            for ((street, number, city, region, postcode), hits) in chunk.iter().zip(&results) {
                let asked = [
                    Some(street.as_str()),
                    number.as_deref(),
                    Some(city.as_str()),
                    region.as_deref(),
                    postcode.as_deref(),
                ]
                .into_iter()
                .flatten()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(", ");
                let row = PyList::empty(py);
                crate::extras::for_query(&asked, || -> PyResult<()> {
                    for h in hits {
                        row.append(hit_dict(py, h, extras.as_ref(), &self.inner)?)?;
                    }
                    Ok(())
                })?;
                out.append(row)?;
            }
        }
        Ok(out)
    }

    /// Reverse geocoding: nearest houses to a point.
    #[pyo3(signature = (lat, lon, k = 3, *, extras = None))]
    fn reverse<'py>(
        &self,
        py: Python<'py>,
        lat: f64,
        lon: f64,
        k: usize,
        extras: Option<&str>,
    ) -> PyResult<Bound<'py, PyList>> {
        let extras = extras_of(extras, &self.inner)?;
        // try_reverse is the single strict entry point: a bad coordinate raises here just
        // as in the CLI/DuckDB, not a silent empty list.
        let hits = py
            .detach(|| self.inner.try_reverse(lat, lon, k))
            .map_err(pyo3::exceptions::PyValueError::new_err)?;
        let out = PyList::empty(py);
        for h in &hits {
            let d = hit_dict(py, h, extras.as_ref(), &self.inner)?;
            if let Some(dm) = h.distance_m {
                d.set_item("distance_m", dm)?;
            }
            out.append(d)?;
        }
        Ok(out)
    }
}

/// The rayon pool, keyed by process id. A bare `OnceLock` survives `os.fork()`
/// "initialized" while the worker threads do not exist in the child — the first
/// `geocode_many` in a forked multiprocessing worker would then queue work nobody
/// runs and hang forever, un-interruptible. Rebuilding the pool
/// when the PID changed makes forked children just work (and re-read
/// GRIDPIN_THREADS for the new process).
fn batch_pool() -> std::sync::Arc<rayon::ThreadPool> {
    use std::sync::{Arc, Mutex};
    static POOL: Mutex<Option<(u32, Arc<rayon::ThreadPool>)>> = Mutex::new(None);
    let pid = std::process::id();
    let mut slot = POOL.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((owner, pool)) = slot.as_ref() {
        if *owner == pid {
            return Arc::clone(pool);
        }
    }
    let n = std::env::var("GRIDPIN_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .map(|v| v.max(1)) // GRIDPIN_THREADS=0 must not build a 0-thread pool
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|c| c.get().saturating_sub(2).max(1))
                .unwrap_or(2)
        });
    let pool = Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .build()
            .unwrap_or_else(|_| {
                rayon::ThreadPoolBuilder::new()
                    .num_threads(1)
                    .build()
                    .unwrap()
            }),
    );
    *slot = Some((pid, Arc::clone(&pool)));
    pool
}

// Sheets of the release (`gridpin sheets`): the same registry, cache folder, check and download as
// the CLI, so a sheet fetched from Python is found by every other GridPin tool. The Python module
// gridpin.sheets wraps these.

fn sheet_of(country: &str) -> PyResult<&'static crate::sheets::Sheet> {
    crate::sheets::resolve(country)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))
}

fn io_err(e: anyhow::Error) -> PyErr {
    pyo3::exceptions::PyOSError::new_err(format!("{e:#}"))
}

/// The release the sheets belong to, and the download base URL.
#[pyfunction]
fn _sheets_release() -> (String, String) {
    let r = crate::sheets::registry();
    (r.release.clone(), r.base_url.clone())
}

/// Every sheet of the release: code, name, file, size, sha256, url.
#[pyfunction]
fn _sheets_registry<'py>(py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
    let out = PyList::empty(py);
    for s in &crate::sheets::registry().sheets {
        let d = PyDict::new(py);
        d.set_item("code", &s.code)?;
        d.set_item("name", &s.name)?;
        d.set_item("file", &s.file)?;
        d.set_item("size", s.size)?;
        d.set_item("sha256", &s.sha256)?;
        d.set_item("url", s.url())?;
        out.append(d)?;
    }
    Ok(out)
}

/// Folder of downloaded sheets (GRIDPIN_HOME, else XDG_CACHE_HOME/gridpin, else ~/.cache/gridpin).
#[pyfunction]
fn _sheets_home() -> PyResult<PathBuf> {
    crate::sheets::home().map_err(io_err)
}

/// Where the sheet of a country lives in the cache, downloaded or not.
#[pyfunction]
fn _sheets_path(country: &str) -> PyResult<PathBuf> {
    crate::sheets::local_path(sheet_of(country)?).map_err(io_err)
}

/// The file is in the cache with the pinned size (the digest is `_sheets_verify`'s job).
#[pyfunction]
fn _sheets_downloaded(country: &str) -> PyResult<bool> {
    crate::sheets::is_downloaded(sheet_of(country)?).map_err(io_err)
}

/// "ok", "missing", "wrong_size" or "wrong_sha256" for the country's sheet at `path` (default: the cache).
#[pyfunction]
#[pyo3(signature = (country, path = None))]
fn _sheets_verify(py: Python<'_>, country: &str, path: Option<PathBuf>) -> PyResult<&'static str> {
    let sheet = sheet_of(country)?;
    let path = match path {
        Some(p) => p,
        None => crate::sheets::local_path(sheet).map_err(io_err)?,
    };
    let verdict = py
        .detach(|| crate::sheets::verify(&path, sheet))
        .map_err(io_err)?;
    Ok(match verdict {
        crate::sheets::Verdict::Ok => "ok",
        crate::sheets::Verdict::Missing => "missing",
        crate::sheets::Verdict::WrongSize(_) => "wrong_size",
        crate::sheets::Verdict::WrongDigest(_) => "wrong_sha256",
    })
}

/// Download the country's sheet unless it is already in place and checked; returns its path.
/// `offline` (or GRIDPIN_OFFLINE) forbids the network: then a missing sheet is an error naming the URL.
#[pyfunction]
#[pyo3(signature = (country, offline = false))]
fn _sheets_fetch(py: Python<'_>, country: &str, offline: bool) -> PyResult<PathBuf> {
    let sheet = sheet_of(country)?;
    let offline = offline || crate::sheets::offline_from_env();
    py.detach(|| crate::sheets::fetch(sheet, offline))
        .map_err(io_err)
}

/// The CSV columns `gridpin batch --with <spec>` adds, in order (the pandas layer names its columns by them).
#[pyfunction]
fn _extras_columns(spec: &str) -> PyResult<Vec<&'static str>> {
    crate::extras::With::parse(spec)
        .map(|w| w.columns())
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))
}

/// Build a sheet of one's own places from a table (name, lat, lon[, group][, code][, aliases]), as
/// `gridpin build --places`; returns the number of places. Open it with `Geocoder(sheet, also=out)`.
#[pyfunction]
#[pyo3(signature = (places, out, license = None))]
fn build_places(
    py: Python<'_>,
    places: PathBuf,
    out: PathBuf,
    license: Option<String>,
) -> PyResult<usize> {
    py.detach(|| crate::places::build(&places, &out, license.as_deref()))
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("{e:#}")))
}

#[pymodule]
fn _gridpin(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Geocoder>()?;
    m.add_function(wrap_pyfunction!(_extras_columns, m)?)?;
    m.add_function(wrap_pyfunction!(build_places, m)?)?;
    m.add_function(wrap_pyfunction!(_sheets_release, m)?)?;
    m.add_function(wrap_pyfunction!(_sheets_registry, m)?)?;
    m.add_function(wrap_pyfunction!(_sheets_home, m)?)?;
    m.add_function(wrap_pyfunction!(_sheets_path, m)?)?;
    m.add_function(wrap_pyfunction!(_sheets_downloaded, m)?)?;
    m.add_function(wrap_pyfunction!(_sheets_verify, m)?)?;
    m.add_function(wrap_pyfunction!(_sheets_fetch, m)?)?;
    Ok(())
}
