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
    Ok(d)
}

#[pymethods]
impl Geocoder {
    #[new]
    #[pyo3(signature = (index_path, poi = None))]
    fn new(index_path: PathBuf, poi: Option<PathBuf>) -> PyResult<Self> {
        let inner = query::Index::open_address(&index_path)
            .map_err(|e| pyo3::exceptions::PyIOError::new_err(e.to_string()))?;
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
    #[pyo3(signature = (q, k = 10))]
    fn geocode<'py>(&self, py: Python<'py>, q: &str, k: usize) -> PyResult<Bound<'py, PyList>> {
        let hits = py.detach(|| query::query_cascade(&self.inner, self.poi.as_ref(), q, k));
        let out = PyList::empty(py);
        for h in &hits {
            out.append(hit_to_dict(py, h)?)?;
        }
        Ok(out)
    }

    /// Batch of queries (rayon; defaults to cores minus two to keep the machine
    /// responsive; override with the GRIDPIN_THREADS env var — it is read when the
    /// pool for the current process is first built, so set it before the first call).
    /// Returns one list of up to `k` hits per query — the same shape as `geocode`,
    /// so `k` is honoured (an empty list means no match). Interruptible: Ctrl-C is
    /// checked between chunks and raises KeyboardInterrupt.
    #[pyo3(signature = (queries, k = 1))]
    fn geocode_many<'py>(
        &self,
        py: Python<'py>,
        queries: Vec<String>,
        k: usize,
    ) -> PyResult<Bound<'py, PyList>> {
        use rayon::prelude::*;
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
            for hits in &results {
                let row = PyList::empty(py);
                for h in hits {
                    row.append(hit_to_dict(py, h)?)?;
                }
                out.append(row)?;
            }
        }
        Ok(out)
    }

    /// Reverse geocoding: nearest houses to a point.
    #[pyo3(signature = (lat, lon, k = 3))]
    fn reverse<'py>(
        &self,
        py: Python<'py>,
        lat: f64,
        lon: f64,
        k: usize,
    ) -> PyResult<Bound<'py, PyList>> {
        // try_reverse is the single strict entry point: a bad coordinate raises here just
        // as in the CLI/DuckDB, not a silent empty list.
        let hits = py
            .detach(|| self.inner.try_reverse(lat, lon, k))
            .map_err(pyo3::exceptions::PyValueError::new_err)?;
        let out = PyList::empty(py);
        for h in &hits {
            let d = hit_to_dict(py, h)?;
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

#[pymodule]
fn gridpin(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Geocoder>()?;
    Ok(())
}
