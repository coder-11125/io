use crate::pricing::{cache_rates_for_model, get_pricing_for_model, ModelPricing};
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
    /// The file's content immediately before this call changed it, when
    /// `tool_name` is `write`/`edit` — used by `rewind::run` to restore
    /// files when the user reverts to an earlier point in the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_edit_snapshot: Option<crate::tools::FileSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnUsage {
    /// Input tokens billed at the full rate, summed over the turn's model
    /// calls. Excludes prompt-cache traffic, which is tracked separately.
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// Input tokens served from the prompt cache, summed over the turn.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cache_read_tokens: u32,
    /// Input tokens written to the prompt cache, summed over the turn.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cache_write_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl TurnUsage {
    pub fn new(input_tokens: u32, output_tokens: u32) -> Self {
        Self {
            input_tokens,
            output_tokens,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost: None,
        }
    }

    /// Record prompt-cache traffic. Call before [`with_cost`](Self::with_cost)
    /// so the cost reflects it.
    pub fn with_cache(mut self, read_tokens: u32, write_tokens: u32) -> Self {
        self.cache_read_tokens = read_tokens;
        self.cache_write_tokens = write_tokens;
        self
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
            self.cost = Some(pricing.calculate_cost_with_cache(
                self.input_tokens,
                self.output_tokens,
                self.cache_read_tokens,
                self.cache_write_tokens,
                cache_rates_for_model(provider, model),
            ));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_usage_cost_includes_cache_traffic() {
        let usage = TurnUsage::new(1000, 500)
            .with_cache(10_000, 2_000)
            .with_cost("anthropic", "claude-sonnet-4-20250514", None);
        // Same figures as the pricing test: 0.021.
        assert!((usage.cost.unwrap() - 0.021).abs() < 1e-9);
    }

    #[test]
    fn turn_usage_cost_uses_the_providers_own_cache_discount() {
        // gpt-4o: $0.0025/1k in, $0.01/1k out, cache reads at 0.5x input:
        //   1*0.0025 + 0.5*0.01 + 10*0.0025*0.5 = 0.0025 + 0.005 + 0.0125 = 0.02
        let usage = TurnUsage::new(1000, 500)
            .with_cache(10_000, 0)
            .with_cost("openai", "gpt-4o", None);
        assert!((usage.cost.unwrap() - 0.02).abs() < 1e-9);

        // Unknown cache pricing falls back to the plain input rate (never
        // understating): gpt-3.5-turbo 0.0005/0.0015:
        //   1*0.0005 + 0.5*0.0015 + 10*0.0005 = 0.0005 + 0.00075 + 0.005
        let usage = TurnUsage::new(1000, 500).with_cache(10_000, 0).with_cost(
            "openai",
            "gpt-3.5-turbo",
            None,
        );
        assert!((usage.cost.unwrap() - 0.00625).abs() < 1e-9);
    }

    #[test]
    fn turn_usage_serde_omits_zero_cache_fields_and_reads_old_records() {
        let plain = serde_json::to_value(TurnUsage::new(10, 5)).unwrap();
        assert!(plain.get("cache_read_tokens").is_none());
        assert!(plain.get("cache_write_tokens").is_none());

        let cached = serde_json::to_value(TurnUsage::new(10, 5).with_cache(7, 3)).unwrap();
        assert_eq!(cached["cache_read_tokens"], 7);
        assert_eq!(cached["cache_write_tokens"], 3);

        // Sessions persisted before caching existed have no cache fields.
        let old: TurnUsage =
            serde_json::from_str(r#"{"input_tokens":10,"output_tokens":5,"cost":0.1}"#).unwrap();
        assert_eq!((old.cache_read_tokens, old.cache_write_tokens), (0, 0));
    }
}
