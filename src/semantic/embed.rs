//! Text embedding behind a trait, with two implementations.
//!
//! Borrowed from ae (`~/code/lib/rust/ae/src/embed.rs`) — keep in sync.
//!
//! [`OnnxEmbedder`] runs the real `all-MiniLM-L6-v2` model (int8-quantized
//! ONNX, fetched from the HuggingFace Hub on first use — see [`onnx`]) via ONNX
//! Runtime. It's the default when the model loads. [`HashEmbedder`] is a
//! deterministic, dependency-free feature-hash fallback used when the model
//! can't be loaded (offline + uncached, missing asset) and in unit tests.
//!
//! Both yield a native-width vector that the MRL pipeline truncates to 64 dims;
//! the trait deliberately does *not* fix the width. [`default_embedder`] picks
//! the best available at runtime, so callers never branch on which one.

mod onnx;

pub(crate) use onnx::OnnxEmbedder;
// Unlike OnnxEmbedder, `Workload` does escape: semantic/mod.rs re-exports it
// and `Session::new` takes one. The lint can't see past a re-export chain, so
// it flags this link anyway.
#[allow(unreachable_pub)]
pub use onnx::Workload;

use super::mrl::MRL_DIMS;

/// Native width of the [`HashEmbedder`] fallback. The MRL stage only needs at
/// least [`super::mrl::MRL_DIMS`] coordinates, so embedders may differ in width.
pub(crate) const EMBED_DIMS: usize = 384;

/// Produces an embedding (length ≥ [`super::mrl::MRL_DIMS`]) for a chunk of text.
pub(crate) trait Embedder: Send + Sync {
    fn embed(&self, text: &str) -> Vec<f32>;

    /// Stable identifier for diagnostics: `"onnx"` for the real model,
    /// `"hash"` for the deterministic fallback.
    fn kind(&self) -> &'static str;
}

/// The best embedder available: the ONNX model if it loads, else the hash
/// fallback. `model` is an optional `--model` request (path or name).
pub(crate) fn default_embedder(model: Option<&str>, workload: Workload) -> Box<dyn Embedder> {
    // Built once per worker thread (see the thread_local pool in `Session::new`),
    // so announce the chosen backend only on the first construction in the
    // process — otherwise `-v` repeats this line once per core. (A small local
    // divergence from ae, which never runs under a verbose logger.)
    static ANNOUNCED: std::sync::Once = std::sync::Once::new();
    match OnnxEmbedder::load(model, workload) {
        Some(e) => {
            ANNOUNCED.call_once(|| log::info!("using ONNX embedder ({} dims)", e.dims()));
            Box::new(e)
        }
        None => {
            ANNOUNCED.call_once(|| {
                log::warn!(
                    "embedding model failed to load — using hash fallback \
                     (--model <dir|.onnx|org/name> to point at one)"
                )
            });
            Box::new(HashEmbedder::new())
        }
    }
}

/// Deterministic, dependency-free embedder via signed feature hashing over
/// word tokens. Not semantically trained, but stable and similarity-preserving
/// at the lexical level — enough to exercise the vector pipeline.
#[derive(Default, Clone, Copy, Debug)]
pub(crate) struct HashEmbedder;

impl HashEmbedder {
    pub(crate) fn new() -> Self {
        Self
    }
}

/// FNV-1a — a small, fast, deterministic hash (no RNG, stable across runs).
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Split into lowercased alphanumeric tokens.
fn tokenize(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
}

impl Embedder for HashEmbedder {
    fn kind(&self) -> &'static str {
        "hash"
    }

    fn embed(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0.0f32; EMBED_DIMS];
        for token in tokenize(text) {
            let h = fnv1a(token.as_bytes());
            // Hash into the leading MRL_DIMS coords only: the MRL stage keeps
            // the first 64 dims, so a uniform spread over EMBED_DIMS (384) would
            // discard ~5/6 of the signal and collapse short records to zero.
            let idx = (h % MRL_DIMS as u64) as usize;
            let sign = if (h >> 32) & 1 == 0 { 1.0 } else { -1.0 };
            v[idx] += sign;
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedding_has_full_width() {
        assert_eq!(HashEmbedder::new().embed("hello world").len(), EMBED_DIMS);
    }

    #[test]
    fn embedding_is_deterministic() {
        let e = HashEmbedder::new();
        assert_eq!(e.embed("a user account"), e.embed("a user account"));
    }

    #[test]
    fn different_text_gives_different_vectors() {
        let e = HashEmbedder::new();
        assert_ne!(e.embed("users"), e.embed("posts"));
    }
}
