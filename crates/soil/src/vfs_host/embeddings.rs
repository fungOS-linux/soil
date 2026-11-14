//! Host-selected embedding providers for `soil:index` semantic search.
//!
//! Local Model2Vec is the default. A remote provider can be used only when the
//! host operator supplies an endpoint explicitly; a Spore never chooses a
//! model, endpoint, token, or network route.

#[cfg(feature = "model2vec")]
use std::sync::Mutex;

#[cfg(feature = "model2vec")]
use anyhow::Context;
#[cfg(feature = "remote")]
use anyhow::anyhow;

/// One embedding model behind a text-in/vector-out boundary.
pub trait EmbeddingProvider: Send + Sync {
    /// Stable cache key. Change it whenever the model or normalization changes.
    fn model_id(&self) -> &str;
    fn dimension(&self) -> usize;
    fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>>;
}

/// Tiny static Model2Vec model. Text-only, fast, and fully local after the
/// first model pull.
#[cfg(feature = "model2vec")]
pub struct Model2VecEmbedder {
    model: Mutex<model2vec_rs::model::StaticModel>,
    model_id: String,
    dimension: usize,
}

#[cfg(feature = "model2vec")]
impl Model2VecEmbedder {
    pub fn potion_base_8m() -> anyhow::Result<Self> {
        Self::from_pretrained("minishlab/potion-base-8M")
    }

    pub fn from_pretrained(repo: &str) -> anyhow::Result<Self> {
        let model = model2vec_rs::model::StaticModel::from_pretrained(repo, None, None, None)
            .with_context(|| format!("load Model2Vec model {repo}"))?;
        let dimension = model.dim();
        Ok(Self {
            model: Mutex::new(model),
            model_id: format!("model2vec:{repo}"),
            dimension,
        })
    }

    pub fn from_path(path: impl AsRef<std::path::Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let model = model2vec_rs::model::StaticModel::from_pretrained(path, None, None, None)
            .with_context(|| format!("load Model2Vec model {}", path.display()))?;
        let dimension = model.dim();
        Ok(Self {
            model: Mutex::new(model),
            model_id: format!("model2vec:{}", path.display()),
            dimension,
        })
    }
}

#[cfg(feature = "model2vec")]
impl EmbeddingProvider for Model2VecEmbedder {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dimension(&self) -> usize {
        self.dimension
    }

    fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let model = self.model.lock().expect("embedding model poisoned");
        Ok(model.encode_with_args(texts, Some(512), 32))
    }
}

/// Opt-in OpenAI-compatible `/v1/embeddings` client. UMIE or any compatible
/// gateway can sit behind this; the host supplies the URL, model, and token.
#[cfg(feature = "remote")]
pub struct RemoteEmbeddingProvider {
    endpoint: String,
    model: String,
    api_key: Option<String>,
    dimension: usize,
    model_id: String,
}

#[cfg(feature = "remote")]
impl RemoteEmbeddingProvider {
    pub fn new(
        endpoint: impl Into<String>,
        model: impl Into<String>,
        api_key: Option<String>,
        dimension: usize,
    ) -> anyhow::Result<Self> {
        let endpoint = endpoint.into();
        let model = model.into();
        if endpoint.is_empty() || model.is_empty() || dimension == 0 {
            return Err(anyhow!(
                "remote embedding endpoint, model, and dimension are required"
            ));
        }
        Ok(Self {
            model_id: format!("remote:{model}"),
            endpoint,
            model,
            api_key,
            dimension,
        })
    }
}

#[cfg(feature = "remote")]
impl EmbeddingProvider for RemoteEmbeddingProvider {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dimension(&self) -> usize {
        self.dimension
    }

    fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let body = serde_json::json!({
            "model": self.model,
            "input": texts,
        });
        let mut request = ureq::post(&self.endpoint).set("content-type", "application/json");
        if let Some(key) = &self.api_key {
            request = request.set("authorization", &format!("Bearer {key}"));
        }
        let response: serde_json::Value = request
            .send_json(body)
            .with_context(|| format!("embedding request to {}", self.endpoint))?
            .into_json()
            .context("decode embedding response")?;
        let data = response
            .get("data")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| anyhow!("embedding response has no data array"))?;
        let mut out = Vec::with_capacity(data.len());
        for item in data {
            let vector = item
                .get("embedding")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| anyhow!("embedding item has no embedding"))?;
            if vector.len() != self.dimension {
                return Err(anyhow!(
                    "embedding dimension {} does not match configured {}",
                    vector.len(),
                    self.dimension
                ));
            }
            out.push(
                vector
                    .iter()
                    .map(|value| value.as_f64().map(|value| value as f32))
                    .collect::<Option<Vec<_>>>()
                    .ok_or_else(|| anyhow!("embedding contains a non-number"))?,
            );
        }
        if out.len() != texts.len() {
            return Err(anyhow!(
                "embedding response returned {} vectors for {} inputs",
                out.len(),
                texts.len()
            ));
        }
        Ok(out)
    }
}

#[cfg(all(test, feature = "model2vec"))]
mod tests {
    use super::*;

    #[test]
    #[ignore = "downloads and loads the potion-base-8M model"]
    fn potion_base_8m_produces_a_unit_vector() {
        let embedder = Model2VecEmbedder::potion_base_8m().unwrap();
        assert_eq!(embedder.dimension(), 256);
        let vectors = embedder.embed(&["a greece trip".to_string()]).unwrap();
        assert_eq!(vectors.len(), 1);
        assert_eq!(vectors[0].len(), 256);
        let norm = vectors[0].iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-3, "norm {norm}");
    }
}
