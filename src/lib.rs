//! gqls — GraphQL schema search.
//!
//! Layered like `rq`: a source loader turns a schema — an SDL file, an
//! introspection JSON dump, or a live endpoint — into flat
//! [`model::SchemaRecord`]s, and a search layer ranks them. Two capabilities are *borrowed* rather than rebuilt:
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
pub(crate) mod logging;
pub mod model;
pub(crate) mod paths;
pub(crate) mod profile;
pub(crate) mod render;
pub(crate) mod resolve;
pub mod search;
pub(crate) mod style;

#[cfg(feature = "_semantic")]
pub(crate) mod semantic;
