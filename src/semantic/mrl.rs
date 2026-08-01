//! Vector truncation to a fixed prefix width. Named (and borrowed verbatim)
//! from ae (`~/code/lib/rust/ae/src/mrl.rs`) — keep in sync.
//!
//! The MRL name is inherited, and overstates the theory: `all-MiniLM-L6-v2` is
//! *not* Matryoshka-trained (contrastive learning over 1B sentence pairs, 2021;
//! its model card claims no truncation support). So this is plain prefix
//! truncation — which, measured, works anyway for what gqls asks of it.
//!
//! On a 10168-record schema, 23 labelled queries: retrieval AUC is 0.982 at 64
//! dims against 0.991 at the full 384, so *ranking* survives truncation nearly
//! intact and 64 dims costs ~6× less cache. What truncation does cost is
//! *calibration* — the mean cosine of unanswerable queries climbs from 0.367
//! (384-d) to 0.510 (64-d) while real answers barely move (0.673 → 0.696), as
//! fewer dimensions crowd everything toward everything. Below ~256 dims an
//! absolute "is this hit any good?" threshold isn't meaningful; relative
//! ranking is. Ranking is all we use it for, so 64 stands.

/// The compressed embedding width every stored/queried vector is reduced to.
pub const MRL_DIMS: usize = 64;

/// Truncate `raw_embedding` to the leading [`MRL_DIMS`] coordinates and
/// L2-normalize it onto the unit sphere.
///
/// # Panics
/// Panics if `raw_embedding` has fewer than [`MRL_DIMS`] elements.
pub fn compress_matryoshka_vector(raw_embedding: &[f32]) -> Vec<f32> {
    assert!(
        raw_embedding.len() >= MRL_DIMS,
        "embedding has {} dims, need at least {MRL_DIMS}",
        raw_embedding.len()
    );

    let truncated = &raw_embedding[0..MRL_DIMS];

    let norm = truncated.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        truncated.iter().map(|x| x / norm).collect()
    } else {
        truncated.to_vec()
    }
}

/// Cosine similarity between two equal-length vectors. Returns `0.0` if either
/// is degenerate or the lengths differ.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0;
    let mut na = 0.0;
    let mut nb = 0.0;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    let denom = na.sqrt() * nb.sqrt();
    if denom > 0.0 {
        dot / denom
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(len: usize) -> Vec<f32> {
        (0..len).map(|i| i as f32 + 1.0).collect()
    }

    #[test]
    fn truncates_to_64_dims() {
        assert_eq!(compress_matryoshka_vector(&ramp(384)).len(), MRL_DIMS);
    }

    #[test]
    fn output_is_unit_norm() {
        let out = compress_matryoshka_vector(&ramp(384));
        let norm = out.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "norm was {norm}");
    }

    #[test]
    fn zero_vector_is_returned_unnormalized() {
        assert_eq!(compress_matryoshka_vector(&vec![0.0; 64]), vec![0.0; 64]);
    }

    #[test]
    fn cosine_of_identical_unit_vectors_is_one() {
        let v = compress_matryoshka_vector(&ramp(64));
        assert!((cosine_similarity(&v, &v) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn cosine_of_orthogonal_vectors_is_zero() {
        let mut a = vec![0.0; 64];
        let mut b = vec![0.0; 64];
        a[0] = 1.0;
        b[1] = 1.0;
        assert!(cosine_similarity(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn cosine_of_mismatched_lengths_is_zero() {
        assert_eq!(cosine_similarity(&[1.0, 2.0], &[1.0]), 0.0);
    }
}
