//! Real *semantic* search with SQL Anywhere: a worked example of plugging a
//! neural sentence-transformer into the [`sqlanywhere::Embedder`] trait.
//!
//! It runs `all-MiniLM-L6-v2` (384-dim) locally with candle — pure Rust, no C++
//! runtime — fetching the weights from the Hugging Face Hub on first run. The
//! embeddings flow into the *exact same* `vector32(...)` + `vector_top_k` path
//! as the built-in `embed()`; only the vectors are smarter.
//!
//! The point: the query below shares **no content words** with the best answer
//! ("a small feline rested on a rug" vs "the cat sat on the mat"). A lexical
//! embedder cannot connect them; a semantic one ranks it first.
//!
//! ```sh
//! cargo run --release        # from examples/semantic-search/
//! ```
//!
//! ## Swapping in a different backend
//!
//! Only [`SentenceTransformer`] is candle-specific. To use ONNX Runtime (`ort`)
//! or a hosted API (OpenAI, Cohere, …) instead, implement [`Embedder`] the same
//! way — return the model's vector from `embed()` and everything else is
//! unchanged.

use anyhow::{Context, Result};
use candle_core::{Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config, DTYPE};
use hf_hub::{api::sync::Api, Repo, RepoType};
use sqlanywhere::{params, Builder, Connection, Embedder};
use tokenizers::Tokenizer;

const MODEL_ID: &str = "sentence-transformers/all-MiniLM-L6-v2";
const DIMS: usize = 384;

/// A local sentence-transformer that implements [`Embedder`].
struct SentenceTransformer {
    model: BertModel,
    tokenizer: Tokenizer,
    device: Device,
}

impl SentenceTransformer {
    /// Load the model + tokenizer, downloading and caching them on first use.
    fn load() -> Result<Self> {
        let device = Device::Cpu;
        let repo = Api::new()?.repo(Repo::new(MODEL_ID.to_string(), RepoType::Model));

        let config: Config =
            serde_json::from_slice(&std::fs::read(repo.get("config.json")?)?)?;
        let tokenizer = Tokenizer::from_file(repo.get("tokenizer.json")?)
            .map_err(anyhow::Error::msg)
            .context("loading tokenizer")?;
        let weights = repo.get("model.safetensors")?;
        let vb = unsafe { VarBuilder::from_mmaped_safetensors(&[weights], DTYPE, &device)? };
        let model = BertModel::load(vb, &config)?;

        Ok(Self { model, tokenizer, device })
    }

    fn encode(&self, text: &str) -> Result<Vec<f32>> {
        let enc = self
            .tokenizer
            .encode(text, true)
            .map_err(anyhow::Error::msg)?;

        let ids = Tensor::new(enc.get_ids(), &self.device)?.unsqueeze(0)?;
        let type_ids = ids.zeros_like()?;
        let mask = Tensor::new(enc.get_attention_mask(), &self.device)?.unsqueeze(0)?;

        // Token embeddings: [1, seq_len, 384].
        let hidden = self.model.forward(&ids, &type_ids, Some(&mask))?;

        // Mean-pool over real (unmasked) tokens, then L2-normalize — the standard
        // sentence-transformers recipe.
        let mask_f = mask.to_dtype(DTYPE)?.unsqueeze(2)?; // [1, seq, 1]
        let summed = hidden.broadcast_mul(&mask_f)?.sum(1)?; // [1, 384]
        let counts = mask_f.sum(1)?; // [1, 1]
        let mean = summed.broadcast_div(&counts)?;
        let norm = mean.sqr()?.sum_keepdim(1)?.sqrt()?;
        let normalized = mean.broadcast_div(&norm)?;

        Ok(normalized.squeeze(0)?.to_vec1::<f32>()?)
    }
}

impl Embedder for SentenceTransformer {
    fn dims(&self) -> usize {
        DIMS
    }

    fn embed(&self, text: &str) -> Vec<f32> {
        // The trait signature is infallible; surface model errors loudly rather
        // than silently returning a zero vector.
        self.encode(text)
            .unwrap_or_else(|e| panic!("embedding failed for {text:?}: {e}"))
    }
}

async fn add(conn: &Connection, model: &SentenceTransformer, id: i64, body: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO docs (id, body, emb) VALUES (?, ?, vector32(?))",
        params![id, body, model.embed_literal(body)],
    )
    .await?;
    Ok(())
}

async fn search(conn: &Connection, model: &SentenceTransformer, query: &str) -> Result<Vec<String>> {
    let mut rows = conn
        .query(
            "SELECT d.body FROM vector_top_k('docs_vec', vector32(?), 3) k \
             JOIN docs d ON d.id = k.id",
            params![model.embed_literal(query)],
        )
        .await?;
    let mut hits = Vec::new();
    while let Some(row) = rows.next().await? {
        hits.push(row.get::<String>(0)?);
    }
    Ok(hits)
}

#[tokio::main]
async fn main() -> Result<()> {
    eprintln!("Loading {MODEL_ID} (first run downloads ~90 MB)…");
    let model = SentenceTransformer::load().context("loading model")?;

    let db = Builder::new_local(":memory:").build().await?;
    let conn = db.connect()?;
    conn.execute(
        &format!("CREATE TABLE docs (id INTEGER PRIMARY KEY, body TEXT, emb FLOAT32({DIMS}))"),
        (),
    )
    .await?;
    conn.execute(
        "CREATE INDEX docs_vec ON docs(sqlanywhere_vector_idx(emb, 'metric=cosine'))",
        (),
    )
    .await?;

    for (id, body) in [
        "the cat sat on the mat",
        "a dog chased a ball across the park",
        "the stock market fell sharply today",
        "she baked a loaf of sourdough bread",
    ]
    .into_iter()
    .enumerate()
    {
        add(&conn, &model, id as i64 + 1, body).await?;
    }

    // No content words shared with "the cat sat on the mat" — only meaning.
    let query = "a small feline rested on a rug";
    println!("\nQuery: {query:?}");
    println!("Top matches (semantic):");
    for (rank, hit) in search(&conn, &model, query).await?.into_iter().enumerate() {
        println!("  {}. {hit}", rank + 1);
    }
    Ok(())
}
