# Semantic search example

A worked example of plugging a **real neural embedding model** into SQL Anywhere
through the [`Embedder`](../../sqlanywhere/src/embed.rs) trait — the recommended
way to get production-grade semantic search.

It runs [`sentence-transformers/all-MiniLM-L6-v2`](https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2)
(384-dim) locally with [candle](https://github.com/huggingface/candle) — pure
Rust, no C++ runtime. Weights are downloaded from the Hugging Face Hub on first
run (~90 MB) and cached.

```sh
cd examples/semantic-search
cargo run --release
```

Expected output:

```text
Query: "a small feline rested on a rug"
Top matches (semantic):
  1. the cat sat on the mat
  2. she baked a loaf of sourdough bread
  3. a dog chased a ball across the park
```

The query shares **no content words** with the best answer — "feline"/"cat",
"rug"/"mat", "rested"/"sat" are only related by *meaning*. The built-in lexical
`embed()` cannot connect them; a semantic model ranks the right document first.
That is the entire reason to bring your own embedder.

## Why this is a separate crate

This example carries heavy ML dependencies (candle, tokenizers, hf-hub). It is
deliberately kept **outside the main Cargo workspace** (see `exclude` in the
root `Cargo.toml`, plus its own empty `[workspace]` table) so those dependencies
never touch the core `sqlanywhere` build or CI. The core stays lean; you opt in
here.

## Using a different backend

Only the `SentenceTransformer` struct in `src/main.rs` is candle-specific.
Everything else — the table, the `vector32(...)` inserts, `vector_top_k` — is
identical regardless of how the vectors are produced. To swap backends,
implement `Embedder` against:

- **ONNX Runtime** via the [`ort`](https://crates.io/crates/ort) crate — load an
  exported sentence-transformer `.onnx` + tokenizer.
- **A hosted API** (OpenAI, Cohere, Voyage, …) — `embed()` just does an HTTP call
  and returns the returned vector.

```rust
use sqlanywhere::Embedder;

struct OpenAiEmbedder { /* http client, api key */ }
impl Embedder for OpenAiEmbedder {
    fn dims(&self) -> usize { 1536 }
    fn embed(&self, text: &str) -> Vec<f32> {
        // POST text to /v1/embeddings, return the vector
        # unimplemented!()
    }
}
```

Declare the `FLOAT32(n)` column to match your model's `dims()`.
