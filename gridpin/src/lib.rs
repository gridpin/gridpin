//! Geocoder core library (used by both the CLI binary and the Python bindings).

pub mod builder;
mod de;
#[cfg(test)]
mod de_effect_audit;
pub mod index;
pub mod ml;
pub mod norm;
pub mod query;
pub mod rules;

#[cfg(feature = "python")]
mod py;
