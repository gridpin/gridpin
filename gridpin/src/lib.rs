//! Geocoder core library (used by both the CLI binary and the Python bindings).

pub mod builder;
pub mod index;
pub mod ml;
pub mod norm;
pub mod query;
pub mod rules;

#[cfg(feature = "python")]
mod py;
