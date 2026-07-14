//! Text embedding: a pluggable [`Embedder`] interface plus a dependency-free
//! reference implementation.
//!
//! An embedding turns text into a fixed-dimension vector so it can be stored in
//! a `FLOAT32(n)` column and searched with the DiskANN index. SQL Anywhere does
//! not force a particular model on you:
//!
//! - [`LexicalEmbedder`] (and the [`embed`] shortcut) is a **zero-dependency**
//!   default — great for prototyping, tests, and hybrid search alongside FTS5.
//!   It is *lexical*, not semantic (see below).
//! - The [`Embedder`] trait lets you **bring your own** real, semantic model
//!   (local ONNX/candle, or a hosted API) and feed it into the exact same
//!   `vector32(...)` insert/search path. The index and `vector_top_k` behave
//!   identically regardless of how the vectors were produced.
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
//! ## Bringing your own (semantic) embedder
//!
//! Implement [`Embedder::embed`] to return the raw vector; the default
//! [`Embedder::embed_literal`] formats it for `vector32`:
//!
//! ```rust
//! use sqlanywhere::Embedder;
//!
//! struct MyModel;
//! impl Embedder for MyModel {
//!     fn dims(&self) -> usize { 384 }
//!     fn embed(&self, text: &str) -> Vec<f32> {
//!         // call your local model or hosted API here…
//!         # let _ = text;
//!         vec![0.0; 384]
//!     }
//! }
//!
//! let literal = MyModel.embed_literal("hello world"); // "[…]" for vector32(?)
//! # let _ = literal;
//! ```
//!
//! ## What kind of embedding is the reference one?
//!
//! [`LexicalEmbedder`] uses the classic **hashing trick** (feature hashing):
//! text is tokenized into words, each word is hashed into one of `dims` buckets
//! with a signed contribution, and the resulting bag-of-words vector is
//! L2-normalized. Documents that share vocabulary get similar vectors, so cosine
//! similarity works as a *lexical* similarity signal.
//!
//! It is **not** a neural/semantic embedding: it has no understanding of
//! synonyms or context. For production semantic search, plug in a real model via
//! the [`Embedder`] trait. The hash is a fixed FNV-1a, so output is stable across
//! platforms and Rust versions.

/// Something that turns text into a fixed-dimension embedding vector.
///
/// Implement this to plug a real semantic model into the same `vector32(...)`
/// storage and [`vector_top_k`](crate) search path used by the built-in
/// [`LexicalEmbedder`].
pub trait Embedder {
    /// The dimensionality of the vectors this embedder produces. Use this to
    /// declare the matching `FLOAT32(n)` column.
    fn dims(&self) -> usize;

    /// Embed `text` into a raw vector of length [`dims`](Embedder::dims).
    fn embed(&self, text: &str) -> Vec<f32>;

    /// Embed `text` and format it as a vector literal (e.g. `"[0.13,-0.51,…]"`)
    /// ready to pass to the `vector32` SQL function.
    fn embed_literal(&self, text: &str) -> String {
        to_vector_literal(&self.embed(text))
    }
}

/// Format a raw vector as a `vector32`-compatible literal such as
/// `"[0.130000,-0.510000]"` (six decimal places).
pub fn to_vector_literal(v: &[f32]) -> String {
    let mut out = String::with_capacity(v.len() * 8 + 2);
    out.push('[');
    for (i, x) in v.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!("{x:.6}"));
    }
    out.push(']');
    out
}

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

/// The zero-dependency reference embedder: the lexical hashing trick over word
/// tokens, producing L2-normalized vectors of a fixed dimensionality.
///
/// See the [module documentation](self) for what this embedder is and is not.
#[derive(Debug, Clone, Copy)]
pub struct LexicalEmbedder {
    dims: usize,
}

impl LexicalEmbedder {
    /// Create a lexical embedder producing `dims`-dimensional vectors (`dims` is
    /// clamped to at least 1).
    pub fn new(dims: usize) -> Self {
        Self { dims: dims.max(1) }
    }
}

impl Embedder for LexicalEmbedder {
    fn dims(&self) -> usize {
        self.dims
    }

    fn embed(&self, text: &str) -> Vec<f32> {
        let dims = self.dims;
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
        v
    }
}

/// Embed `text` into a `dims`-dimensional, L2-normalized vector literal such as
/// `"[0.13,-0.51,...]"`, ready to pass to the `vector32` SQL function.
///
/// This is a shortcut for [`LexicalEmbedder::new(dims).embed_literal(text)`].
/// `dims` is clamped to at least 1; empty or token-less input yields an all-zero
/// vector. See the [module documentation](self) for what this embedder is and is
/// not, and how to plug in a real semantic model.
pub fn embed(text: &str, dims: usize) -> String {
    LexicalEmbedder::new(dims).embed_literal(text)
}

#[cfg(test)]
mod tests {
    use super::{embed, to_vector_literal, Embedder, LexicalEmbedder};

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

    #[test]
    fn free_fn_matches_trait() {
        let e = LexicalEmbedder::new(32);
        assert_eq!(embed("same output", 32), e.embed_literal("same output"));
        assert_eq!(e.dims(), 32);
    }

    #[test]
    fn to_literal_formats_six_decimals() {
        assert_eq!(to_vector_literal(&[0.5, -0.25]), "[0.500000,-0.250000]");
    }

    #[test]
    fn custom_embedder_via_trait() {
        // A trivial "bring your own model" embedder plugs into the same path.
        struct Ones(usize);
        impl Embedder for Ones {
            fn dims(&self) -> usize {
                self.0
            }
            fn embed(&self, _text: &str) -> Vec<f32> {
                vec![1.0; self.0]
            }
        }
        assert_eq!(
            Ones(3).embed_literal("anything"),
            "[1.000000,1.000000,1.000000]"
        );
    }
}
