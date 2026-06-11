use super::anthropic::{build_messages, parse_and_convert_response};
use super::{CompletionModel, CompletionRequest, CompletionResponse, StreamEvent};
use crate::config::BedrockConfig;
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
        let creds = self.credentials()?;
        let region = self.region();
        let service = "bedrock";

        let host = format!("bedrock-runtime.{region}.amazonaws.com");
        let path = format!("/model/{}/invoke", self.config.model);
        let url = format!("https://{host}{path}");

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

        let resp = req.send().await?;

        if !resp.status().is_success() {
            return Err(super::api_error("Bedrock", resp).await);
        }

        parse_and_convert_response(&resp.text().await?)
    }

    async fn complete_stream(
        &self,
        _request: CompletionRequest,
    ) -> anyhow::Result<tokio::sync::mpsc::Receiver<anyhow::Result<StreamEvent>>> {
        anyhow::bail!("Bedrock streaming not yet implemented")
    }
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
