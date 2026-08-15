//! The real embedder: `all-MiniLM-L6-v2` (int8-quantized ONNX) via ONNX
//! Runtime, with mean pooling over the token embeddings.
//!
//! Borrowed from ae (`~/code/lib/rust/ae/src/embed/onnx.rs`) — keep in sync.
//!
//! Resolution when no explicit `--model` is requested: `$GQLS_MODEL_DIR` (a
//! local dir holding `model.onnx` + `tokenizer.json`) → the model fetched from
//! the HuggingFace Hub into the shared cache (`~/.cache/huggingface/hub`). If
//! nothing loads (offline + uncached), callers use the hash fallback.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::Tensor;
use tokenizers::Tokenizer;

use super::Embedder;

/// all-MiniLM-L6-v2 hidden size.
const NATIVE_DIMS: usize = 384;
/// Cap sequence length — short jargon phrases never need the full context.
const MAX_SEQ: usize = 256;
/// Default model on the HuggingFace Hub (ONNX int8-quantized export of
/// all-MiniLM-L6-v2). Override with `--model <dir | .onnx | org/name>`.
const DEFAULT_HF_REPO: &str = "Xenova/all-MiniLM-L6-v2";
const HF_MODEL_FILE: &str = "onnx/model_quantized.onnx";
const HF_TOKENIZER_FILE: &str = "tokenizer.json";

pub(crate) struct OnnxEmbedder {
    session: Mutex<Session>,
    tokenizer: Tokenizer,
}

impl OnnxEmbedder {
    /// Load the embedder. `spec` is an optional `--model` request: a path (to a
    /// model dir or `.onnx` file) or a bare `org/name` Hub id. Returns `None`
    /// (→ hash fallback) if nothing usable loads.
    pub(crate) fn load(spec: Option<&str>, workload: Workload) -> Option<Self> {
        ensure_ort_dylib();

        if let Some(spec) = spec {
            return match resolve(spec).and_then(|(m, t)| Self::from_files(&m, &t, workload)) {
                Some(e) => Some(e),
                None => {
                    log::debug!("--model {spec}: could not load");
                    None
                }
            };
        }

        if let Some(dir) = std::env::var_os("GQLS_MODEL_DIR") {
            let dir = PathBuf::from(dir);
            return Self::from_files(
                &dir.join("model.onnx"),
                &dir.join("tokenizer.json"),
                workload,
            );
        }
        let (model, tokenizer) = fetch_from_hub(DEFAULT_HF_REPO)?;
        Self::from_files(&model, &tokenizer, workload)
    }

    fn from_files(model: &Path, tokenizer: &Path, workload: Workload) -> Option<Self> {
        let model = std::fs::read(model).ok()?;
        let tokenizer = std::fs::read(tokenizer).ok()?;
        Self::from_bytes(&model, &tokenizer, workload)
    }

    fn from_bytes(model: &[u8], tokenizer: &[u8], workload: Workload) -> Option<Self> {
        if model.is_empty() {
            return None;
        }
        let tokenizer = Tokenizer::from_bytes(tokenizer)
            .map_err(|e| log::debug!("tokenizer load failed: {e}"))
            .ok()?;
        let session = build_session(model, workload)
            .map_err(|e| log::debug!("ONNX session load failed: {e}"))
            .ok()?;
        Some(Self {
            session: Mutex::new(session),
            tokenizer,
        })
    }

    pub(crate) fn dims(&self) -> usize {
        NATIVE_DIMS
    }

    /// Tokenize, run the model, and mean-pool — `None` on any error so the
    /// public [`Embedder::embed`] can degrade to a zero vector.
    fn run(&self, text: &str) -> Option<Vec<f32>> {
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(|e| log::warn!("tokenize failed: {e}"))
            .ok()?;

        let take = encoding.get_ids().len().min(MAX_SEQ);
        let ids: Vec<i64> = encoding.get_ids()[..take]
            .iter()
            .map(|&x| x as i64)
            .collect();
        let mask: Vec<i64> = encoding.get_attention_mask()[..take]
            .iter()
            .map(|&x| x as i64)
            .collect();
        let type_ids = vec![0i64; take];
        let shape = [1_i64, take as i64];

        let ids_t = Tensor::from_array((shape, ids)).ok()?;
        let mask_t = Tensor::from_array((shape, mask.clone())).ok()?;
        let type_t = Tensor::from_array((shape, type_ids)).ok()?;

        let mut session = self.session.lock().unwrap();
        let outputs = session
            .run(ort::inputs![
                "input_ids" => ids_t,
                "attention_mask" => mask_t,
                "token_type_ids" => type_t,
            ])
            .map_err(|e| log::warn!("inference failed: {e}"))
            .ok()?;

        let (shape, data) = outputs[0].try_extract_tensor::<f32>().ok()?;
        let seq = shape[1] as usize;
        let hidden = shape[2] as usize;
        Some(mean_pool(data, &mask, seq, hidden))
    }
}

impl Embedder for OnnxEmbedder {
    fn kind(&self) -> &'static str {
        "onnx"
    }

    fn embed(&self, text: &str) -> Vec<f32> {
        self.run(text).unwrap_or_else(|| vec![0.0; NATIVE_DIMS])
    }
}

/// What a session is about to be used for. It decides the model's own
/// threading, and the two answers are opposite — measured on a 10k-record
/// schema, one query embed is 21.9ms single-threaded against 8.7ms on four,
/// while a whole-schema embed is 121s single-threaded against 133s on four.
#[derive(Clone, Copy, PartialEq)]
pub enum Workload {
    /// One query at a time: nothing else is running, so give ONNX the cores.
    Query,
    /// A whole schema: rayon already runs an inference per core, and intra-op
    /// threads on top of that only oversubscribe.
    Bulk,
}

impl Workload {
    fn intra_threads(self) -> usize {
        match self {
            // Four, not every core: the gain has flattened by then (8.4ms on
            // eight against 8.7ms on four), and the spare cores are what let a
            // background warm run alongside without a fight.
            Workload::Query => std::thread::available_parallelism()
                .map(|n| n.get().min(4))
                .unwrap_or(1),
            Workload::Bulk => 1,
        }
    }
}

fn build_session(model: &[u8], workload: Workload) -> ort::Result<Session> {
    Session::builder()?
        .with_optimization_level(GraphOptimizationLevel::Level3)?
        .with_intra_threads(workload.intra_threads())?
        .commit_from_memory(model)
}

/// Resolve a `--model` spec to a `(model.onnx, tokenizer.json)` pair.
fn resolve(spec: &str) -> Option<(PathBuf, PathBuf)> {
    let path = Path::new(spec);
    if path.is_dir() {
        return Some((path.join("model.onnx"), path.join("tokenizer.json")));
    }
    if path.is_file() {
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        return Some((path.to_path_buf(), dir.join("tokenizer.json")));
    }
    if spec.contains('/')
        && !spec.starts_with('.')
        && !spec.starts_with('/')
        && !spec.starts_with('~')
    {
        return fetch_from_hub(spec);
    }
    None
}

/// Fetch `<repo>`'s ONNX model + tokenizer from the HuggingFace Hub into the
/// shared cache, returning their local paths. `None` on any error.
fn fetch_from_hub(repo: &str) -> Option<(PathBuf, PathBuf)> {
    use hf_hub::api::sync::Api;
    let api = Api::new()
        .map_err(|e| log::debug!("HuggingFace Hub init failed: {e}"))
        .ok()?;
    let repo = api.model(repo.to_string());
    let model = repo
        .get(HF_MODEL_FILE)
        .map_err(|e| log::debug!("model fetch failed: {e}"))
        .ok()?;
    let tokenizer = repo
        .get(HF_TOKENIZER_FILE)
        .map_err(|e| log::debug!("tokenizer fetch failed: {e}"))
        .ok()?;
    Some((model, tokenizer))
}

/// Under the load-dynamic strategy (`semantic-dynamic`, the Homebrew build),
/// ONNX Runtime is dlopen'd at runtime from `ORT_DYLIB_PATH`. If it isn't set,
/// probe the usual install locations (the Homebrew keg in particular) so a
/// packaged gqls works with no env setup. A no-op for the static build.
fn ensure_ort_dylib() {
    #[cfg(feature = "semantic-dynamic")]
    {
        if std::env::var_os("ORT_DYLIB_PATH").is_some() {
            return;
        }
        let lib = if cfg!(target_os = "macos") {
            "libonnxruntime.dylib"
        } else {
            "libonnxruntime.so"
        };
        let mut bases: Vec<PathBuf> = Vec::new();
        if let Some(prefix) = std::env::var_os("HOMEBREW_PREFIX") {
            let prefix = PathBuf::from(prefix);
            bases.push(prefix.join("opt/onnxruntime/lib"));
            bases.push(prefix.join("lib"));
        }
        for p in [
            "/opt/homebrew/opt/onnxruntime/lib",
            "/opt/homebrew/lib",
            "/usr/local/lib",
            "/usr/lib",
        ] {
            bases.push(PathBuf::from(p));
        }
        if let Some(dir) = bases.into_iter().find(|d| d.join(lib).is_file()) {
            // SAFETY: called once at embedder load, before any ORT use.
            unsafe { std::env::set_var("ORT_DYLIB_PATH", dir.join(lib)) };
        }
    }
}

/// Mean-pool `[1, seq, hidden]` token embeddings, weighted by the attention
/// mask, into one `hidden`-length vector.
fn mean_pool(data: &[f32], mask: &[i64], seq: usize, hidden: usize) -> Vec<f32> {
    let mut pooled = vec![0.0f32; hidden];
    let mut count = 0.0f32;
    for t in 0..seq {
        if mask.get(t).copied().unwrap_or(0) == 0 {
            continue;
        }
        count += 1.0;
        let row = &data[t * hidden..(t + 1) * hidden];
        for (p, &x) in pooled.iter_mut().zip(row) {
            *p += x;
        }
    }
    if count > 0.0 {
        for p in &mut pooled {
            *p /= count;
        }
    }
    pooled
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mean_pool_ignores_masked_positions() {
        let data = [1.0, 2.0, 9.0, 9.0];
        let mask = [1, 0];
        assert_eq!(mean_pool(&data, &mask, 2, 2), vec![1.0, 2.0]);
    }

    #[test]
    fn mean_pool_averages_active_positions() {
        let data = [1.0, 1.0, 3.0, 3.0];
        let mask = [1, 1];
        assert_eq!(mean_pool(&data, &mask, 2, 2), vec![2.0, 2.0]);
    }
}
