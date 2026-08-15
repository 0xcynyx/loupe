//! Milvus access layer, the only module that talks to the Milvus RESTful v2 API.

use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Debug)]
pub struct MilvusError(pub String);

impl std::fmt::Display for MilvusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for MilvusError {}

type Result<T> = std::result::Result<T, MilvusError>;

// Contract the API layer depends on, decoupled from the REST transport.
#[async_trait]
pub trait MilvusApi: Send + Sync {
    async fn list_collections(&self) -> Result<Vec<String>>;
    async fn describe(&self, name: &str) -> Result<Value>;
    async fn row_count(&self, name: &str) -> Result<i64>;
    async fn load_state(&self, name: &str) -> Result<String>;
    async fn query(&self, name: &str, filter: &str, output_fields: &[String], limit: usize, offset: usize) -> Result<Vec<Value>>;
    async fn count(&self, name: &str, filter: &str) -> Result<i64>;
    async fn load(&self, name: &str) -> Result<()>;
    async fn release(&self, name: &str) -> Result<()>;
}

pub struct RestClient {
    base: String,
    auth: String,
    http: reqwest::Client,
}

impl RestClient {
    pub fn new(host: &str, port: &str, user: &str, password: &str) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|e| MilvusError(e.to_string()))?;
        Ok(Self {
            base: format!("http://{host}:{port}/v2/vectordb"),
            auth: format!("Bearer {user}:{password}"),
            http,
        })
    }

    // Posts one REST call and unwraps the {code, message, data} envelope.
    async fn post(&self, path: &str, payload: Value) -> Result<Value> {
        let url = format!("{}{}", self.base, path);
        let response = self
            .http
            .post(&url)
            .header("Authorization", &self.auth)
            .json(&payload)
            .send()
            .await
            .map_err(|e| MilvusError(format!("request to {url} failed: {e}")))?;
        let body: Value = response
            .json()
            .await
            .map_err(|e| MilvusError(format!("invalid JSON from {url}: {e}")))?;
        let code = body["code"].as_i64().unwrap_or(-1);
        if code != 0 {
            let message = body["message"].as_str().unwrap_or("unknown Milvus error");
            return Err(MilvusError(format!("Milvus code {code}: {message}")));
        }
        Ok(body["data"].clone())
    }
}

#[async_trait]
impl MilvusApi for RestClient {
    async fn list_collections(&self) -> Result<Vec<String>> {
        let data = self.post("/collections/list", json!({})).await?;
        let names = data
            .as_array()
            .map(|items| items.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        Ok(names)
    }

    async fn describe(&self, name: &str) -> Result<Value> {
        self.post("/collections/describe", json!({"collectionName": name})).await
    }

    async fn row_count(&self, name: &str) -> Result<i64> {
        let data = self.post("/collections/get_stats", json!({"collectionName": name})).await?;
        Ok(data["rowCount"].as_i64().unwrap_or(0))
    }

    async fn load_state(&self, name: &str) -> Result<String> {
        let data = self.post("/collections/get_load_state", json!({"collectionName": name})).await?;
        Ok(data["loadState"].as_str().unwrap_or("unknown").to_string())
    }

    async fn query(&self, name: &str, filter: &str, output_fields: &[String], limit: usize, offset: usize) -> Result<Vec<Value>> {
        let payload = json!({
            "collectionName": name,
            "filter": filter,
            "outputFields": output_fields,
            "limit": limit,
            "offset": offset,
        });
        let data = self.post("/entities/query", payload).await?;
        Ok(data.as_array().cloned().unwrap_or_default())
    }

    async fn count(&self, name: &str, filter: &str) -> Result<i64> {
        let payload = json!({"collectionName": name, "filter": filter, "outputFields": ["count(*)"], "limit": 1});
        let data = self.post("/entities/query", payload).await?;
        Ok(data[0]["count(*)"].as_i64().unwrap_or(0))
    }

    async fn load(&self, name: &str) -> Result<()> {
        self.post("/collections/load", json!({"collectionName": name})).await.map(|_| ())
    }

    async fn release(&self, name: &str) -> Result<()> {
        self.post("/collections/release", json!({"collectionName": name})).await.map(|_| ())
    }
}
