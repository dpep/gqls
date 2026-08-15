//! Vector truncation to a fixed prefix width. Named (and borrowed verbatim)
//! from ae (`~/code/lib/rust/ae/src/mrl.rs`) — keep in sync.
//!
//! The MRL name is inherited, and overstates the theory: `all-MiniLM-L6-v2` is
//! *not* Matryoshka-trained (contrastive learning over 1B sentence pairs, 2021;
//! its model card claims no truncation support). So this is plain prefix
//! truncation — which, measured, works anyway for what gqls asks of it.
//!
//! What truncation costs is *calibration*, not ranking. On a 10168-record
//! schema, 23 labelled queries: retrieval AUC is 0.982 at 64 dims against 0.991
//! at the full 384. But the mean cosine of unanswerable queries climbs from
//! 0.367 (384-d) to 0.510 (64-d) while real answers barely move (0.673 →
//! 0.696), because fewer dimensions crowd everything toward everything.
//!
//! That only mattered while ranking was all gqls asked of the vectors. There's
//! an absolute floor now — a query the schema can't answer returns nothing —
//! and a floor needs the two populations to separate. Measured on a
//! 4602-record schema, top cosine over six answerable and six nonsense queries:
//!
//! | dims | answerable, lowest | nonsense, highest | gap   | vectors |
//! |------|--------------------|-------------------|-------|---------|
//! | 64   | 0.560              | 0.478             | 0.082 | 1.2 MB  |
//! | 256  | 0.526              | 0.309             | 0.217 | 4.8 MB  |
//! | 384  | 0.483              | 0.324             | 0.159 | 7.2 MB  |
//!
//! 256 separates best *and* costs less than 384 — past it the extra dimensions
//! pull the weakest real answers down faster than they push nonsense away.
//! [`super::RELEVANCE_FLOOR`] sits in the middle of its gap.
//!
//! The cost is real: four times the vectors on disk, and everyone re-embeds
//! once, since the width is part of the cache key.

/// The compressed embedding width every stored/queried vector is reduced to.
pub(crate) const MRL_DIMS: usize = 256;

/// Truncate `raw_embedding` to the leading [`MRL_DIMS`] coordinates and
/// L2-normalize it onto the unit sphere.
///
/// # Panics
/// Panics if `raw_embedding` has fewer than [`MRL_DIMS`] elements.
pub(crate) fn compress_matryoshka_vector(raw_embedding: &[f32]) -> Vec<f32> {
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
pub(crate) fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
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
    fn truncates_to_the_configured_width() {
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
        assert_eq!(
            compress_matryoshka_vector(&vec![0.0; MRL_DIMS]),
            vec![0.0; MRL_DIMS]
        );
    }

    #[test]
    fn cosine_of_identical_unit_vectors_is_one() {
        let v = compress_matryoshka_vector(&ramp(MRL_DIMS));
        assert!((cosine_similarity(&v, &v) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn cosine_of_orthogonal_vectors_is_zero() {
        let mut a = vec![0.0; MRL_DIMS];
        let mut b = vec![0.0; MRL_DIMS];
        a[0] = 1.0;
        b[1] = 1.0;
        assert!(cosine_similarity(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn cosine_of_mismatched_lengths_is_zero() {
        assert_eq!(cosine_similarity(&[1.0, 2.0], &[1.0]), 0.0);
    }
}
