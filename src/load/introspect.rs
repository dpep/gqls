//! Load a schema by introspecting a live endpoint.
//!
//! TODO(v0.1): POST the standard introspection query to `url`, parse the
//! `__schema` JSON, and flatten it into the same [`SchemaRecord`]s that
//! [`crate::load::sdl`] produces. Needs an HTTP client dep (e.g. `ureq`)
//! plus `serde_json`; left as a stub so the default build stays light. The
//! record model is identical, so only this file changes.

use anyhow::{bail, Result};

use crate::model::SchemaRecord;

pub fn from_url(url: &str) -> Result<Vec<SchemaRecord>> {
    let _ = url;
    bail!(
        "URL/introspection input isn't implemented yet — v0 handles .graphql SDL files. \
         See src/load/introspect.rs"
    )
}
