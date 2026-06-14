/// Pricing per 1,000 tokens in USD (approximate — verify at provider docs)
#[derive(Debug, Clone)]
pub struct ModelPricing {
    pub input_cost_per_1k: f64,
    pub output_cost_per_1k: f64,
}

/// How a provider/model combination handles billing.
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderPricingCategory {
    /// Per-token pricing is known; cost can be calculated.
    Priced,
    /// Self-hosted or local — no API charges.
    Free,
    /// Billed via subscription, not per individual token.
    SubscriptionBased,
    /// Proxy or platform — actual cost depends on the routed backend or deployment config.
    PassThrough,
    /// Provider is known but this specific model has no entry in the pricing table.
    ModelNotInTable,
    /// Provider is not recognised at all.
    ProviderNotInTable,
}

impl ModelPricing {
    pub fn new(input_cost_per_1k: f64, output_cost_per_1k: f64) -> Self {
        Self {
            input_cost_per_1k,
            output_cost_per_1k,
        }
    }

    pub fn calculate_cost(&self, input_tokens: u32, output_tokens: u32) -> f64 {
        (input_tokens as f64 / 1000.0) * self.input_cost_per_1k
            + (output_tokens as f64 / 1000.0) * self.output_cost_per_1k
    }
}

/// Returns the billing category for a (provider, model) pair.
///
/// Use this to explain *why* a cost figure is unavailable rather than silently
/// showing "cost unavailable" for every non-priced provider.
pub fn get_provider_pricing_category(provider: &str, model: &str) -> ProviderPricingCategory {
    match provider {
        // Self-hosted / local — no API cost
        "ollama" | "local" => ProviderPricingCategory::Free,

        // Subscription billing
        "github_copilot" => ProviderPricingCategory::SubscriptionBased,

        // Proxies and deployment platforms — cost depends on the backend or contract
        "openrouter" | "azure" | "opencode_go" | "opencode_zen" => {
            ProviderPricingCategory::PassThrough
        }

        // Priced providers
        _ => match get_pricing_for_model(provider, model) {
            Some(_) => ProviderPricingCategory::Priced,
            None if is_known_provider(provider) => ProviderPricingCategory::ModelNotInTable,
            None => ProviderPricingCategory::ProviderNotInTable,
        },
    }
}

fn is_known_provider(provider: &str) -> bool {
    matches!(
        provider,
        "anthropic"
            | "openai"
            | "gemini"
            | "groq"
            | "xai"
            | "mistral"
            | "deepseek"
            | "bedrock"
            | "ollama"
    )
}

/// Returns per-token pricing for a (provider, model) pair, or `None` if unknown.
///
/// All figures are approximate USD per 1,000 tokens as of mid-2025.
/// Check the provider's pricing page for the latest rates.
pub fn get_pricing_for_model(provider: &str, model: &str) -> Option<ModelPricing> {
    let m = model.to_lowercase();
    match provider {
        "anthropic" => anthropic_pricing(&m),
        "openai" => openai_pricing(&m),
        "gemini" => gemini_pricing(&m),
        "groq" => groq_pricing(&m),
        "xai" => xai_pricing(&m),
        "mistral" => mistral_pricing(&m),
        "deepseek" => deepseek_pricing(&m),
        // Bedrock routes to underlying models; match by model id prefix
        "bedrock" => bedrock_pricing(&m),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Per-provider pricing tables
// All values: USD per 1,000 tokens (input, output)
// ---------------------------------------------------------------------------

fn anthropic_pricing(model: &str) -> Option<ModelPricing> {
    // https://www.anthropic.com/pricing
    Some(match model {
        m if m.contains("claude-opus-4") => ModelPricing::new(0.015, 0.075),
        m if m.contains("claude-sonnet-4")
            || m.contains("claude-3-7-sonnet")
            || m.contains("claude-3-5-sonnet") =>
        {
            ModelPricing::new(0.003, 0.015)
        }
        m if m.contains("claude-haiku-4") || m.contains("claude-3-5-haiku") => {
            ModelPricing::new(0.0008, 0.004)
        }
        m if m.contains("claude-3-opus") => ModelPricing::new(0.015, 0.075),
        m if m.contains("claude-3-sonnet") => ModelPricing::new(0.003, 0.015),
        m if m.contains("claude-3-haiku") => ModelPricing::new(0.00025, 0.00125),
        m if m.contains("claude-2") => ModelPricing::new(0.008, 0.024),
        // Catch-all for future Claude models — use current Sonnet pricing as a conservative estimate
        m if m.contains("claude") => ModelPricing::new(0.003, 0.015),
        _ => return None,
    })
}

fn openai_pricing(model: &str) -> Option<ModelPricing> {
    // https://openai.com/pricing
    Some(match model {
        // GPT-4.1 family (2025)
        m if m.contains("gpt-4.1-nano") => ModelPricing::new(0.0001, 0.0004),
        m if m.contains("gpt-4.1-mini") => ModelPricing::new(0.0004, 0.0016),
        m if m.contains("gpt-4.1") => ModelPricing::new(0.002, 0.008),
        // GPT-4o family
        m if m.contains("gpt-4o-mini") => ModelPricing::new(0.00015, 0.0006),
        m if m.contains("gpt-4o") => ModelPricing::new(0.0025, 0.01),
        // GPT-4 legacy
        m if m.contains("gpt-4-turbo") => ModelPricing::new(0.01, 0.03),
        m if m.starts_with("gpt-4") => ModelPricing::new(0.03, 0.06),
        // GPT-3.5
        m if m.contains("gpt-3.5-turbo") => ModelPricing::new(0.0005, 0.0015),
        // Reasoning models
        m if m.contains("o4-mini") => ModelPricing::new(0.0011, 0.0044),
        m if m.contains("o3-mini") => ModelPricing::new(0.0011, 0.0044),
        m if m.contains("o3") => ModelPricing::new(0.01, 0.04),
        m if m.contains("o1-mini") => ModelPricing::new(0.003, 0.012),
        m if m.contains("o1") => ModelPricing::new(0.015, 0.06),
        _ => return None,
    })
}

fn gemini_pricing(model: &str) -> Option<ModelPricing> {
    // https://ai.google.dev/pricing
    Some(match model {
        // Gemini 2.5
        m if m.contains("gemini-2.5-pro") => ModelPricing::new(0.00125, 0.01),
        m if m.contains("gemini-2.5-flash") => ModelPricing::new(0.00015, 0.0006),
        // Gemini 2.0
        m if m.contains("gemini-2.0-flash-lite") => ModelPricing::new(0.000075, 0.0003),
        m if m.contains("gemini-2.0-flash") => ModelPricing::new(0.0001, 0.0004),
        // Gemini 1.5
        m if m.contains("gemini-1.5-pro") => ModelPricing::new(0.00125, 0.005),
        m if m.contains("gemini-1.5-flash-8b") => ModelPricing::new(0.0000375, 0.00015),
        m if m.contains("gemini-1.5-flash") => ModelPricing::new(0.000075, 0.0003),
        // Gemini 1.0
        m if m.contains("gemini-1.0-pro") || m.contains("gemini-pro") => {
            ModelPricing::new(0.0005, 0.0015)
        }
        // Catch-all
        m if m.contains("gemini") => ModelPricing::new(0.00125, 0.005),
        _ => return None,
    })
}

fn groq_pricing(model: &str) -> Option<ModelPricing> {
    // https://groq.com/pricing/
    Some(match model {
        // Llama 4
        m if m.contains("llama-4-maverick") => ModelPricing::new(0.0005, 0.00077),
        m if m.contains("llama-4-scout") => ModelPricing::new(0.00011, 0.00034),
        // Llama 3.x
        m if m.contains("llama-3.3-70b") => ModelPricing::new(0.00059, 0.00079),
        m if m.contains("llama-3.1-405b") => ModelPricing::new(0.00059, 0.00079),
        m if m.contains("llama-3.1-70b") => ModelPricing::new(0.00059, 0.00079),
        m if m.contains("llama-3.1-8b") => ModelPricing::new(0.00005, 0.00008),
        m if m.contains("llama-3-70b") || m.contains("llama3-70b") => {
            ModelPricing::new(0.00059, 0.00079)
        }
        m if m.contains("llama-3-8b") || m.contains("llama3-8b") => {
            ModelPricing::new(0.00005, 0.00008)
        }
        // DeepSeek on Groq
        m if m.contains("deepseek-r1-distill-llama-70b") => ModelPricing::new(0.00075, 0.00099),
        // Mixtral
        m if m.contains("mixtral-8x7b") => ModelPricing::new(0.00024, 0.00024),
        // Gemma
        m if m.contains("gemma2-9b") || m.contains("gemma-7b") => {
            ModelPricing::new(0.00007, 0.00007)
        }
        // Qwen
        m if m.contains("qwen") => ModelPricing::new(0.00029, 0.00029),
        _ => return None,
    })
}

fn xai_pricing(model: &str) -> Option<ModelPricing> {
    // https://x.ai/api
    Some(match model {
        m if m.contains("grok-3-mini") => ModelPricing::new(0.0003, 0.0005),
        m if m.contains("grok-3") => ModelPricing::new(0.003, 0.015),
        m if m.contains("grok-2-mini") || m.contains("grok-2-vision") => {
            ModelPricing::new(0.0002, 0.002)
        }
        m if m.contains("grok-2") => ModelPricing::new(0.002, 0.01),
        m if m.contains("grok-vision-beta") => ModelPricing::new(0.005, 0.015),
        m if m.contains("grok-beta") => ModelPricing::new(0.005, 0.015),
        m if m.contains("grok") => ModelPricing::new(0.003, 0.015),
        _ => return None,
    })
}

fn mistral_pricing(model: &str) -> Option<ModelPricing> {
    // https://mistral.ai/technology/#pricing
    Some(match model {
        m if m.contains("mistral-large") || m.contains("pixtral-large") => {
            ModelPricing::new(0.002, 0.006)
        }
        m if m.contains("mistral-medium") => ModelPricing::new(0.0027, 0.0081),
        m if m.contains("codestral") => ModelPricing::new(0.0002, 0.0006),
        m if m.contains("mistral-small") => ModelPricing::new(0.0001, 0.0003),
        m if m.contains("mistral-saba") => ModelPricing::new(0.0002, 0.0006),
        m if m.contains("mistral-nemo") || m.contains("open-mistral-nemo") => {
            ModelPricing::new(0.00015, 0.00015)
        }
        m if m.contains("mixtral-8x22b") => ModelPricing::new(0.002, 0.006),
        m if m.contains("mixtral-8x7b") || m.contains("open-mixtral-8x7b") => {
            ModelPricing::new(0.0007, 0.0007)
        }
        m if m.contains("mistral-7b") || m.contains("open-mistral-7b") => {
            ModelPricing::new(0.00025, 0.00025)
        }
        m if m.contains("mistral") => ModelPricing::new(0.001, 0.003),
        _ => return None,
    })
}

fn deepseek_pricing(model: &str) -> Option<ModelPricing> {
    // https://api-docs.deepseek.com/quick_start/pricing
    Some(match model {
        m if m.contains("deepseek-r1") => ModelPricing::new(0.00055, 0.00219),
        m if m.contains("deepseek-v3") => ModelPricing::new(0.00027, 0.0011),
        m if m.contains("deepseek-coder") => ModelPricing::new(0.00014, 0.00028),
        // deepseek-chat maps to DeepSeek-V3 on the API
        m if m.contains("deepseek-chat") || m.contains("deepseek") => {
            ModelPricing::new(0.00027, 0.0011)
        }
        _ => return None,
    })
}

fn bedrock_pricing(model: &str) -> Option<ModelPricing> {
    // Bedrock model IDs are prefixed with provider, e.g. "anthropic.claude-3-sonnet-*"
    // Apply the underlying provider's pricing where identifiable.
    if model.contains("anthropic.") || model.contains("claude") {
        return anthropic_pricing(model);
    }
    if model.contains("mistral.") || model.contains("mistral") {
        return mistral_pricing(model);
    }
    if model.contains("amazon.titan") {
        return Some(match model {
            m if m.contains("titan-text-express") => ModelPricing::new(0.0002, 0.0006),
            m if m.contains("titan-text-lite") => ModelPricing::new(0.00015, 0.0002),
            _ => ModelPricing::new(0.0002, 0.0006),
        });
    }
    if model.contains("amazon.nova") || model.contains("nova-") {
        return Some(match model {
            m if m.contains("nova-pro") => ModelPricing::new(0.0008, 0.0032),
            m if m.contains("nova-lite") => ModelPricing::new(0.00006, 0.00024),
            m if m.contains("nova-micro") => ModelPricing::new(0.000035, 0.00014),
            _ => ModelPricing::new(0.0008, 0.0032),
        });
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TurnUsage;

    #[test]
    fn test_cost_calculation() {
        let pricing = ModelPricing::new(0.003, 0.015);
        let cost = pricing.calculate_cost(1000, 500);
        // (1.0 * 0.003) + (0.5 * 0.015) = 0.003 + 0.0075 = 0.0105
        assert!((cost - 0.0105).abs() < 1e-9);
    }

    #[test]
    fn test_anthropic_known_models() {
        for model in &[
            "claude-opus-4-20250514",
            "claude-sonnet-4-6",
            "claude-haiku-4-5-20251001",
            "claude-3-opus-20240229",
            "claude-3-5-sonnet-20241022",
            "claude-3-haiku-20240307",
        ] {
            assert!(
                get_pricing_for_model("anthropic", model).is_some(),
                "missing pricing for anthropic/{model}"
            );
        }
    }

    #[test]
    fn test_openai_known_models() {
        for model in &[
            "gpt-4o",
            "gpt-4o-mini",
            "gpt-4.1",
            "gpt-4.1-mini",
            "gpt-4.1-nano",
            "gpt-4-turbo",
            "gpt-3.5-turbo",
            "o1",
            "o1-mini",
            "o3",
            "o3-mini",
            "o4-mini",
        ] {
            assert!(
                get_pricing_for_model("openai", model).is_some(),
                "missing pricing for openai/{model}"
            );
        }
    }

    #[test]
    fn test_free_providers() {
        assert_eq!(
            get_provider_pricing_category("ollama", "llama3"),
            ProviderPricingCategory::Free
        );
    }

    #[test]
    fn test_pass_through_providers() {
        for provider in &["openrouter", "azure", "opencode_go", "opencode_zen"] {
            assert_eq!(
                get_provider_pricing_category(provider, "some-model"),
                ProviderPricingCategory::PassThrough,
                "{provider} should be PassThrough"
            );
        }
    }

    #[test]
    fn test_unknown_model() {
        // "totally-unknown-xyz" contains no known model-name substring, so anthropic_pricing
        // returns None and the function falls through to ModelNotInTable.
        assert_eq!(
            get_provider_pricing_category("anthropic", "totally-unknown-xyz"),
            ProviderPricingCategory::ModelNotInTable
        );
    }

    #[test]
    fn test_unknown_provider() {
        assert_eq!(
            get_provider_pricing_category("some-new-provider", "model-x"),
            ProviderPricingCategory::ProviderNotInTable
        );
    }

    #[test]
    fn test_turnusage_with_cost() {
        let usage = TurnUsage::new(1000, 500).with_cost("anthropic", "claude-sonnet-4-20250514");
        assert!(usage.cost.is_some());
        let cost = usage.cost.unwrap();
        // (1.0 * 0.003) + (0.5 * 0.015) = 0.0105
        assert!((cost - 0.0105).abs() < 1e-9);
    }
}
