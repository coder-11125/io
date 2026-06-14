use super::anthropic::{build_messages, parse_and_convert_response, AnthropicStreamState};
use super::{CompletionModel, CompletionRequest, CompletionResponse, StreamEvent};
use crate::config::BedrockConfig;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone)]
pub struct BedrockProvider {
    config: BedrockConfig,
    client: reqwest::Client,
}

struct AwsCredentials {
    access_key_id: String,
    secret_access_key: String,
    session_token: Option<String>,
}

impl BedrockProvider {
    pub fn new(config: BedrockConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .expect("failed to build reqwest client");
        Self { config, client }
    }

    fn region(&self) -> String {
        self.config
            .region
            .clone()
            .or_else(|| std::env::var("AWS_REGION").ok())
            .unwrap_or_else(|| "us-east-1".to_string())
    }

    fn credentials(&self) -> anyhow::Result<AwsCredentials> {
        let access_key_id = std::env::var("AWS_ACCESS_KEY_ID")
            .map_err(|_| anyhow::anyhow!("missing AWS_ACCESS_KEY_ID environment variable"))?;
        let secret_access_key = std::env::var("AWS_SECRET_ACCESS_KEY")
            .map_err(|_| anyhow::anyhow!("missing AWS_SECRET_ACCESS_KEY environment variable"))?;
        let session_token = std::env::var("AWS_SESSION_TOKEN").ok();
        Ok(AwsCredentials {
            access_key_id,
            secret_access_key,
            session_token,
        })
    }

    /// Build a SigV4-signed POST request for the given Bedrock Runtime path and body.
    fn build_signed_request(
        &self,
        path: &str,
        payload: Vec<u8>,
    ) -> anyhow::Result<reqwest::RequestBuilder> {
        let creds = self.credentials()?;
        let region = self.region();
        let service = "bedrock";

        let host = format!("bedrock-runtime.{region}.amazonaws.com");
        let url = format!("https://{host}{path}");
        let payload_hash = sha256_hex(&payload);

        let now = chrono::Utc::now();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date_stamp = now.format("%Y%m%d").to_string();

        let canonical_uri = path
            .split('/')
            .map(urlencode_segment)
            .collect::<Vec<_>>()
            .join("/");
        let canonical_querystring = "";

        let mut signed_headers_list =
            vec!["content-type", "host", "x-amz-content-sha256", "x-amz-date"];
        let mut canonical_headers = format!(
            "content-type:application/json\nhost:{host}\nx-amz-content-sha256:{payload_hash}\nx-amz-date:{amz_date}\n"
        );
        if creds.session_token.is_some() {
            canonical_headers.push_str(&format!(
                "x-amz-security-token:{}\n",
                creds.session_token.as_deref().unwrap()
            ));
            signed_headers_list.push("x-amz-security-token");
        }
        let signed_headers = signed_headers_list.join(";");

        let canonical_request = format!(
            "POST\n{canonical_uri}\n{canonical_querystring}\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
        );

        let algorithm = "AWS4-HMAC-SHA256";
        let credential_scope = format!("{date_stamp}/{region}/{service}/aws4_request");
        let string_to_sign = format!(
            "{algorithm}\n{amz_date}\n{credential_scope}\n{}",
            sha256_hex(canonical_request.as_bytes())
        );

        let k_date = hmac(
            format!("AWS4{}", creds.secret_access_key).as_bytes(),
            date_stamp.as_bytes(),
        );
        let k_region = hmac(&k_date, region.as_bytes());
        let k_service = hmac(&k_region, service.as_bytes());
        let k_signing = hmac(&k_service, b"aws4_request");
        let signature = hex::encode(hmac(&k_signing, string_to_sign.as_bytes()));

        let authorization = format!(
            "{algorithm} Credential={}/{credential_scope}, SignedHeaders={signed_headers}, Signature={signature}",
            creds.access_key_id
        );

        let mut req = self
            .client
            .post(&url)
            .header("content-type", "application/json")
            .header("host", &host)
            .header("x-amz-content-sha256", &payload_hash)
            .header("x-amz-date", &amz_date)
            .header("Authorization", authorization)
            .body(payload);
        if let Some(ref token) = creds.session_token {
            req = req.header("x-amz-security-token", token);
        }
        Ok(req)
    }
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

#[async_trait::async_trait]
impl CompletionModel for BedrockProvider {
    fn provider_name(&self) -> &'static str {
        "bedrock"
    }
    fn context_window(&self) -> u64 {
        self.config
            .context_window
            .unwrap_or_else(|| super::context_window_for_model(&self.config.model))
    }

    async fn complete(&self, request: CompletionRequest) -> anyhow::Result<CompletionResponse> {
        let path = format!("/model/{}/invoke", self.config.model);

        let mut body = serde_json::json!({
            "anthropic_version": "bedrock-2023-05-31",
            "max_tokens": request.max_tokens.unwrap_or(16384),
            "messages": build_messages(&request),
        });
        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        if !request.tools.is_empty() {
            body["tools"] =
                serde_json::json!(request.tools.iter().map(|t| serde_json::json!({
                "name": t.name, "description": t.description, "input_schema": t.input_schema,
            })).collect::<Vec<_>>());
        }
        if let Some(ref system) = request.system_prompt {
            body["system"] = serde_json::json!(system);
        }

        let payload = serde_json::to_vec(&body)?;
        let req = self.build_signed_request(&path, payload)?;
        let resp = req.send().await?;

        if !resp.status().is_success() {
            return Err(super::api_error("Bedrock", resp).await);
        }

        parse_and_convert_response(&resp.text().await?)
    }

    async fn complete_stream(
        &self,
        request: CompletionRequest,
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<StreamEvent>>> {
        let path = format!("/model/{}/invoke-with-response-stream", self.config.model);

        let mut body = serde_json::json!({
            "anthropic_version": "bedrock-2023-05-31",
            "max_tokens": request.max_tokens.unwrap_or(16384),
            "messages": build_messages(&request),
        });
        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        if !request.tools.is_empty() {
            body["tools"] =
                serde_json::json!(request.tools.iter().map(|t| serde_json::json!({
                "name": t.name, "description": t.description, "input_schema": t.input_schema,
            })).collect::<Vec<_>>());
        }
        if let Some(ref system) = request.system_prompt {
            body["system"] = serde_json::json!(system);
        }

        let payload = serde_json::to_vec(&body)?;
        let req = self.build_signed_request(&path, payload)?;
        let resp = req.send().await?;

        if !resp.status().is_success() {
            return Err(super::api_error("Bedrock", resp).await);
        }

        let (tx, rx) = tokio::sync::mpsc::channel(64);

        tokio::spawn(async move {
            use futures::StreamExt;
            let mut stream = resp.bytes_stream();
            let mut buffer: Vec<u8> = Vec::new();
            let mut state = AnthropicStreamState::new();

            'outer: while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(bytes) => {
                        buffer.extend_from_slice(&bytes);
                        loop {
                            match parse_event_frame(&buffer) {
                                Some((headers, payload, frame_len)) => {
                                    buffer = buffer[frame_len..].to_vec();

                                    let msg_type =
                                        headers.get(":message-type").map(|s| s.as_str());
                                    if msg_type == Some("exception") {
                                        let msg = String::from_utf8_lossy(&payload).to_string();
                                        let _ = tx
                                            .send(Err(anyhow::anyhow!("Bedrock error: {msg}")))
                                            .await;
                                        break 'outer;
                                    }

                                    let event_type =
                                        headers.get(":event-type").map(|s| s.as_str());
                                    if event_type != Some("chunk") {
                                        continue;
                                    }

                                    // Payload is {"bytes": "<base64 Anthropic event JSON>"}
                                    let chunk_json = match serde_json::from_slice::<
                                        serde_json::Value,
                                    >(&payload)
                                    {
                                        Ok(v) => v,
                                        Err(e) => {
                                            let _ = tx
                                                .send(Err(anyhow::anyhow!(
                                                    "chunk JSON error: {e}"
                                                )))
                                                .await;
                                            break 'outer;
                                        }
                                    };
                                    let bytes_str =
                                        chunk_json["bytes"].as_str().unwrap_or_default();
                                    let decoded = match BASE64.decode(bytes_str) {
                                        Ok(d) => d,
                                        Err(e) => {
                                            let _ = tx
                                                .send(Err(anyhow::anyhow!(
                                                    "base64 decode: {e}"
                                                )))
                                                .await;
                                            break 'outer;
                                        }
                                    };
                                    let event = match serde_json::from_slice::<serde_json::Value>(
                                        &decoded,
                                    ) {
                                        Ok(v) => v,
                                        Err(e) => {
                                            let _ = tx
                                                .send(Err(anyhow::anyhow!("event parse: {e}")))
                                                .await;
                                            break 'outer;
                                        }
                                    };
                                    match state.process_event(&event) {
                                        Ok((events, done)) => {
                                            for ev in events {
                                                if tx.send(Ok(ev)).await.is_err() {
                                                    break 'outer;
                                                }
                                            }
                                            if done {
                                                break 'outer;
                                            }
                                        }
                                        Err(e) => {
                                            let _ = tx.send(Err(e)).await;
                                            break 'outer;
                                        }
                                    }
                                }
                                None => break, // need more data
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(anyhow::anyhow!("stream error: {e}"))).await;
                        break 'outer;
                    }
                }
            }
        });

        Ok(rx)
    }
}

/// Parse one AWS event-stream binary frame from `data`.
/// Returns `(headers, payload, total_frame_bytes)` or `None` if not enough data.
///
/// Frame layout (all integers big-endian):
///   [total_len: u32][headers_len: u32][prelude_crc: u32]
///   [headers: headers_len bytes][payload: ...][message_crc: u32]
fn parse_event_frame(
    data: &[u8],
) -> Option<(std::collections::HashMap<String, String>, Vec<u8>, usize)> {
    if data.len() < 12 {
        return None;
    }
    let total_len = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let hdrs_len = u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as usize;

    if data.len() < total_len {
        return None;
    }

    // prelude (8) + prelude CRC (4) = headers start at byte 12
    let hdrs_start = 12;
    let hdrs_end = hdrs_start + hdrs_len;
    // strip the 4-byte trailing message CRC
    let payload_end = total_len.saturating_sub(4);

    if hdrs_end > payload_end {
        return None;
    }

    let headers = parse_headers(&data[hdrs_start..hdrs_end]);
    let payload = data[hdrs_end..payload_end].to_vec();
    Some((headers, payload, total_len))
}

/// Parse the AWS event-stream headers block, returning string-valued headers.
/// Non-string typed headers (booleans, integers, timestamps, UUIDs) are skipped.
fn parse_headers(data: &[u8]) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let mut pos = 0;
    while pos < data.len() {
        let name_len = data[pos] as usize;
        pos += 1;
        if pos + name_len > data.len() {
            break;
        }
        let name = String::from_utf8_lossy(&data[pos..pos + name_len]).to_string();
        pos += name_len;
        if pos >= data.len() {
            break;
        }
        let header_type = data[pos];
        pos += 1;
        // Skip bytes consumed by each header type; only store String (7) / Bytes (6) values.
        let skip = match header_type {
            0 | 1 => 0,      // BoolTrue / BoolFalse — no payload bytes
            2 => 1,           // Byte
            3 => 2,           // Short
            4 => 4,           // Int
            5 | 8 => 8,       // Long / Timestamp
            6 | 7 => {
                // Bytes / String: 2-byte length prefix + value
                if pos + 2 > data.len() {
                    break;
                }
                let vlen = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
                if pos + 2 + vlen > data.len() {
                    break;
                }
                let value =
                    String::from_utf8_lossy(&data[pos + 2..pos + 2 + vlen]).to_string();
                out.insert(name, value);
                pos += 2 + vlen;
                continue;
            }
            9 => 16, // UUID
            _ => break,
        };
        pos += skip;
    }
    out
}

fn urlencode_segment(seg: &str) -> String {
    let mut out = String::new();
    for byte in seg.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}
