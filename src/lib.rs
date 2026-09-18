//! gqls — GraphQL schema search.
//!
//! Layered like `rq`: a source loader turns a schema — an SDL file, an
//! introspection JSON dump, or a live endpoint — into flat
//! [`model::SchemaRecord`]s, and a search layer ranks them. Fuzzy ranking is
//! adapted from `rq`'s `search/score.rs` (`~/code/lib/rust/rq`); see
//! [`search::score`].

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
