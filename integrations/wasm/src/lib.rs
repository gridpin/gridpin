//! GridPin in WebAssembly: a few plain exports over linear memory, no wasm-bindgen.
//!
//! `gw_alloc(len)` gives a buffer the caller fills (the sheet's bytes, then each query as UTF-8); `gw_open(ptr, len)`
//! takes the sheet (the buffer is consumed); `gw_geocode(ptr, len, k)` and `gw_reverse(lat, lon, k)` answer with a JSON
//! array whose address and length `gw_result_ptr()` and `gw_result_len()` give until the next call; `gw_free(ptr, len)`
//! frees a buffer the caller no longer needs. One sheet per instance. `gridpin.js` wraps all of it.

use std::cell::RefCell;

use gridpin::query::{query_cascade, Index};

thread_local! {
    static SHEET: RefCell<Option<Index>> = const { RefCell::new(None) };
    static RESULT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

fn set_result(text: String) -> i32 {
    RESULT.with(|r| *r.borrow_mut() = text.into_bytes());
    0
}

#[no_mangle]
pub extern "C" fn gw_alloc(len: usize) -> *mut u8 {
    let mut v = vec![0u8; len];
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p
}

/// # Safety
/// `ptr`, `len` come from `gw_alloc` and are freed once.
#[no_mangle]
pub unsafe extern "C" fn gw_free(ptr: *mut u8, len: usize) {
    drop(Vec::from_raw_parts(ptr, len, len));
}

/// Open the sheet whose bytes are at `ptr`; the buffer is taken over. 0 on success, -1 with the message as result.
///
/// # Safety
/// `ptr`, `len` come from `gw_alloc`; the caller does not free them after this call.
#[no_mangle]
pub unsafe extern "C" fn gw_open(ptr: *mut u8, len: usize) -> i32 {
    let bytes = Vec::from_raw_parts(ptr, len, len);
    match Index::from_bytes(bytes, "sheet.bin") {
        Ok(idx) => {
            SHEET.with(|s| *s.borrow_mut() = Some(idx));
            set_result(String::new())
        }
        Err(e) => {
            set_result(e.to_string());
            -1
        }
    }
}

/// Geocode the UTF-8 query at `ptr`; the answers (a JSON array) are the result. -1 without a sheet.
///
/// # Safety
/// `ptr`, `len` point at `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn gw_geocode(ptr: *const u8, len: usize, k: usize) -> i32 {
    let q = String::from_utf8_lossy(std::slice::from_raw_parts(ptr, len)).into_owned();
    SHEET.with(|s| match s.borrow().as_ref() {
        None => set_result("no sheet: call open first".into()) - 1,
        Some(idx) => {
            let hits = query_cascade(idx, None, &q, k.clamp(1, 10));
            set_result(serde_json::to_string(&hits).unwrap_or_else(|_| "[]".into()))
        }
    })
}

/// The nearest addresses of a point; a JSON array as the result.
#[no_mangle]
pub extern "C" fn gw_reverse(lat: f64, lon: f64, k: usize) -> i32 {
    SHEET.with(|s| match s.borrow().as_ref() {
        None => set_result("no sheet: call open first".into()) - 1,
        Some(idx) => {
            let hits = idx.try_reverse(lat, lon, k.clamp(1, 10)).unwrap_or_default();
            set_result(serde_json::to_string(&hits).unwrap_or_else(|_| "[]".into()))
        }
    })
}

#[no_mangle]
pub extern "C" fn gw_result_ptr() -> *const u8 {
    RESULT.with(|r| r.borrow().as_ptr())
}

#[no_mangle]
pub extern "C" fn gw_result_len() -> usize {
    RESULT.with(|r| r.borrow().len())
}
