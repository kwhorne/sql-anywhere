//! A small, dependency-free reference text embedder.
//!
//! [`embed`] turns a piece of text into a fixed-dimension, L2-normalized vector
//! literal (e.g. `"[0.13,-0.51,...]"`) that plugs straight into the `vector32`
//! SQL function — so you can build a vector column without pre-computing
//! embeddings outside the database:
//!
//! ```rust
//! # async fn run() {
//! use sqlanywhere::{embed, params, Builder};
//!
//! let db = Builder::new_local(":memory:").build().await.unwrap();
//! let conn = db.connect().unwrap();
//! conn.execute("CREATE TABLE docs (id INTEGER PRIMARY KEY, emb FLOAT32(64))", ())
//!     .await
//!     .unwrap();
//!
//! // Embed text inline — no external model call needed for the reference embedder.
//! conn.execute(
//!     "INSERT INTO docs (emb) VALUES (vector32(?))",
//!     params![embed("memory safety and ownership", 64)],
//! )
//! .await
//! .unwrap();
//! # }
//! ```
//!
//! ## What kind of embedding is this?
//!
//! This is the classic **hashing trick** (feature hashing): text is tokenized
//! into words, each word is hashed into one of `dims` buckets with a signed
//! contribution, and the resulting bag-of-words vector is L2-normalized.
//! Documents that share vocabulary get similar vectors, so cosine similarity
//! works as a *lexical* similarity signal — great as a zero-dependency default,
//! for prototyping, and for hybrid search alongside FTS5.
//!
//! It is **not** a neural/semantic embedding: it has no understanding of
//! synonyms or context. For production semantic search, compute embeddings with
//! a real model (local ONNX or a hosted API) and store them the same way — the
//! DiskANN index and `vector_top_k` work identically regardless of how the
//! vectors were produced.
//!
//! The hash is a fixed FNV-1a, so output is stable across platforms and Rust
//! versions.

/// FNV-1a 64-bit hash — small, fast, and deterministic across platforms and
/// compiler versions (unlike `DefaultHasher`).
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Embed `text` into a `dims`-dimensional, L2-normalized vector literal such as
/// `"[0.13,-0.51,...]"`, ready to pass to the `vector32` SQL function.
///
/// Uses the hashing trick over lowercase alphanumeric word tokens. `dims` is
/// clamped to at least 1. Empty or token-less input yields an all-zero vector.
///
/// See the [module documentation](self) for what this embedder is and is not.
pub fn embed(text: &str, dims: usize) -> String {
    let dims = dims.max(1);
    let mut v = vec![0f32; dims];

    for token in text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
    {
        let token = token.to_lowercase();
        let h = fnv1a(token.as_bytes());
        let idx = (h % dims as u64) as usize;
        // Use a separate bit of the hash for the sign so collisions can cancel.
        let sign = if (h >> 63) & 1 == 0 { 1.0 } else { -1.0 };
        v[idx] += sign;
    }

    // L2-normalize so cosine distance is well-behaved.
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }

    let mut out = String::with_capacity(dims * 8);
    out.push('[');
    for (i, x) in v.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!("{:.6}", x));
    }
    out.push(']');
    out
}

#[cfg(test)]
mod tests {
    use super::embed;

    fn parse(s: &str) -> Vec<f32> {
        s.trim_start_matches('[')
            .trim_end_matches(']')
            .split(',')
            .map(|x| x.parse().unwrap())
            .collect()
    }

    fn dot(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| x * y).sum()
    }

    #[test]
    fn deterministic_and_correct_shape() {
        let a = embed("hello world", 16);
        let b = embed("hello world", 16);
        assert_eq!(a, b, "embedding must be deterministic");
        assert_eq!(parse(&a).len(), 16);
    }

    #[test]
    fn normalized_unit_length() {
        let v = parse(&embed("the quick brown fox jumps", 32));
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4, "expected unit norm, got {norm}");
    }

    #[test]
    fn similar_text_more_similar_than_unrelated() {
        // Shared vocabulary -> higher cosine similarity than unrelated text.
        let base = parse(&embed("rust memory safety and ownership", 256));
        let similar = parse(&embed("ownership and memory safety in rust", 256));
        let unrelated = parse(&embed("a recipe for chocolate cake", 256));
        assert!(
            dot(&base, &similar) > dot(&base, &unrelated),
            "similar text should score higher"
        );
    }

    #[test]
    fn empty_input_is_zero_vector() {
        let v = parse(&embed("", 8));
        assert!(v.iter().all(|&x| x == 0.0));
    }

    #[test]
    fn dims_clamped_to_at_least_one() {
        assert_eq!(parse(&embed("x", 0)).len(), 1);
    }
}
