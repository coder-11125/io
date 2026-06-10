use super::openai::{
    build_chat_body_with_model, parse_and_convert_chat_response, parse_and_convert_chunk,
};
use super::{CompletionModel, CompletionRequest, CompletionResponse, StreamEvent};
use crate::config::AzureConfig;

#[derive(Debug, Clone)]
pub struct AzureProvider {
    config: AzureConfig,
    client: reqwest::Client,
}

impl AzureProvider {
    pub fn new(config: AzureConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .expect("failed to build reqwest client");
        Self { config, client }
    }

    fn api_key(&self) -> anyhow::Result<String> {
        if let Some(ref key) = self.config.api_key {
            return Ok(key.clone());
        }
        let env_var = self
            .config
            .api_key_env
            .as_deref()
            .unwrap_or("AZURE_OPENAI_API_KEY");
        std::env::var(env_var)
            .map_err(|_| anyhow::anyhow!("missing {env_var} environment variable"))
    }

    fn url(&self) -> anyhow::Result<String> {
        let endpoint = self
            .config
            .endpoint
            .clone()
            .or_else(|| std::env::var("AZURE_OPENAI_ENDPOINT").ok())
            .ok_or_else(|| {
                anyhow::anyhow!("Azure endpoint not set — set AZURE_OPENAI_ENDPOINT or config")
            })?;
        Ok(format!(
            "{}/openai/deployments/{}/chat/completions?api-version={}",
            endpoint.trim_end_matches('/'),
            self.config.deployment,
            self.config.api_version
        ))
    }
}

#[async_trait::async_trait]
impl CompletionModel for AzureProvider {
    fn provider_name(&self) -> &'static str {
        "azure"
    }
    fn context_window(&self) -> u64 {
        super::context_window_for_model(&self.config.deployment)
    }

    async fn complete(&self, request: CompletionRequest) -> anyhow::Result<CompletionResponse> {
        let api_key = self.api_key()?;
        let url = self.url()?;
        let body = build_chat_body_with_model(&self.config.deployment, &request, false);

        let resp = self
            .client
            .post(url)
            .header("api-key", api_key)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Azure API error ({status}): {text}");
        }

        parse_and_convert_chat_response(&resp.text().await?)
    }

    async fn complete_stream(
        &self,
        request: CompletionRequest,
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<StreamEvent>>> {
        let api_key = self.api_key()?;
        let url = self.url()?;
        let body = build_chat_body_with_model(&self.config.deployment, &request, true);

        let resp = self
            .client
            .post(url)
            .header("api-key", api_key)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Azure API error ({status}): {text}");
        }

        let (tx, rx) = tokio::sync::mpsc::channel(64);

        tokio::spawn(async move {
            use futures::StreamExt;
            let mut stream = resp.bytes_stream();
            let mut buffer = String::new();

            while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(bytes) => {
                        buffer.push_str(&String::from_utf8_lossy(&bytes));
                        while let Some(line_end) = buffer.find('\n') {
                            let line = buffer[..line_end].trim().to_string();
                            buffer = buffer[line_end + 1..].to_string();
                            if line.is_empty() {
                                continue;
                            }
                            if !line.starts_with("data: ") {
                                continue;
                            }
                            let data = &line[6..];
                            if data == "[DONE]" {
                                let _ = tx
                                    .send(Ok(StreamEvent {
                                        delta: None,
                                        content_block: None,
                                        stop_reason: Some("stop".to_string()),
                                        usage: None,
                                    }))
                                    .await;
                                return;
                            }
                            match parse_and_convert_chunk(data) {
                                Ok(event) => {
                                    if tx.send(Ok(event)).await.is_err() {
                                        return;
                                    }
                                }
                                Err(e) => {
                                    let _ = tx.send(Err(anyhow::anyhow!("parse error: {e}"))).await;
                                    return;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(anyhow::anyhow!("stream error: {e}"))).await;
                        return;
                    }
                }
            }
        });

        Ok(rx)
    }
}
