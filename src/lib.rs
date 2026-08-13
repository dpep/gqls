//! gqls — GraphQL schema search.
//!
//! Layered like `rq`: a source loader turns a schema (SDL file, or — planned
//! — an introspection URL) into flat [`model::SchemaRecord`]s, and a search
//! layer ranks them. Two capabilities are *borrowed* rather than rebuilt:
//!
//! - fuzzy ranking — adapted from `rq`'s `search/score.rs`
//!   (`~/code/lib/rust/rq`); see [`search::score`].
//! - semantic search — lifted from `ae`'s local embedding pipeline
//!   (`~/code/lib/rust/ae`: `embed.rs` + `mrl.rs`); see [`semantic`].
//!   Behind the `semantic` cargo feature (pulls in ONNX Runtime).

pub mod cli;
pub mod example;
pub mod load;
#[macro_use]
pub mod logging;
pub mod model;
pub mod paths;
pub mod profile;
pub mod resolve;
pub mod search;
pub mod style;

#[cfg(feature = "_semantic")]
pub mod semantic;
