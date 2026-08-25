use crate::pricing::{get_pricing_for_model, ModelPricing};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(Uuid);

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for SessionId {
    type Err = uuid::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::from_str(s).map(SessionId)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub turns: Vec<Turn>,
    pub metadata: SessionMetadata,
    /// Summary injected into context after a /compact. Replaces the cleared turns.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub project_root: Option<String>,
    pub model: String,
    pub provider: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub user_message: String,
    pub assistant_message: Option<String>,
    pub tool_calls: Vec<ToolCallRecord>,
    pub usage: Option<TurnUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub tool_name: String,
    pub input: serde_json::Value,
    pub output: String,
    pub success: bool,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
}

impl TurnUsage {
    pub fn new(input_tokens: u32, output_tokens: u32) -> Self {
        Self {
            input_tokens,
            output_tokens,
            cost: None,
        }
    }

    /// `pricing_override` (from `ProviderConfig::pricing_override_for`) takes
    /// precedence over the static `pricing.rs` table when set — real,
    /// per-model pricing discovered from a provider catalog beats a guess.
    pub fn with_cost(
        mut self,
        provider: &str,
        model: &str,
        pricing_override: Option<ModelPricing>,
    ) -> Self {
        if let Some(pricing) = pricing_override.or_else(|| get_pricing_for_model(provider, model)) {
            self.cost = Some(pricing.calculate_cost(self.input_tokens, self.output_tokens));
        }
        self
    }
}

impl Session {
    pub fn new(model: String, provider: String) -> Self {
        let now = Utc::now();
        Self {
            id: SessionId::new(),
            created_at: now,
            updated_at: now,
            turns: Vec::new(),
            metadata: SessionMetadata {
                project_root: None,
                model,
                provider,
            },
            summary: None,
        }
    }

    pub fn add_turn(&mut self, turn: Turn) {
        self.turns.push(turn);
        self.updated_at = Utc::now();
    }

    pub fn recent_turns(&self, n: usize) -> &[Turn] {
        let len = self.turns.len();
        if len <= n {
            &self.turns
        } else {
            &self.turns[len - n..]
        }
    }
}
