//! The `/cost` report: per-turn token usage and API cost for the session.

pub async fn cost_lines(agent: &io_runtime::Agent) -> anyhow::Result<Vec<String>> {
    // The live session: a new one is not in the database until its first turn.
    let session = agent.session_snapshot().await;
    let session_id = session.id;

    let provider = &session.metadata.provider;
    let model = &session.metadata.model;
    let pricing_category = io_runtime::get_provider_pricing_category(provider, model);

    let mut out: Vec<String> = Vec::new();
    let mut total_input_tokens: u32 = 0;
    let mut total_output_tokens: u32 = 0;
    let mut total_cache_read_tokens: u32 = 0;
    let mut total_cache_write_tokens: u32 = 0;
    let mut total_cost: f64 = 0.0;
    let mut priced_turns: usize = 0;
    let mut missing_cost_turns: usize = 0;

    out.push("--- Cost Summary ---".to_string());
    out.push(format!("Session:  {}", &session_id.to_string()[..8]));
    out.push(format!("Provider: {provider}"));
    out.push(format!("Model:    {model}"));
    out.push(format!("Turns:    {}", session.turns.len()));
    out.push(String::new());

    if session.turns.is_empty() {
        out.push("No turns in this session yet.".to_string());
        out.push(String::new());
        return Ok(out);
    }

    let no_cost_label = match pricing_category {
        io_runtime::ProviderPricingCategory::Free => "free / self-hosted",
        io_runtime::ProviderPricingCategory::SubscriptionBased => "subscription billing",
        io_runtime::ProviderPricingCategory::PassThrough => "proxy — cost via backend",
        io_runtime::ProviderPricingCategory::ModelNotInTable => "model not in pricing table",
        io_runtime::ProviderPricingCategory::ProviderNotInTable => "provider not in pricing table",
        io_runtime::ProviderPricingCategory::Priced => "cost unavailable",
    };

    for (i, turn) in session.turns.iter().enumerate() {
        match &turn.usage {
            None => {
                out.push(format!("Turn {:>3}: no token data recorded", i + 1));
            }
            Some(usage) => {
                total_input_tokens += usage.input_tokens;
                total_output_tokens += usage.output_tokens;
                total_cache_read_tokens += usage.cache_read_tokens;
                total_cache_write_tokens += usage.cache_write_tokens;
                // Prompt-cache traffic is billed apart from "in", so show it.
                let cache_note = if usage.cache_read_tokens > 0 || usage.cache_write_tokens > 0 {
                    format!(
                        " [cache: {} read, {} write]",
                        usage.cache_read_tokens, usage.cache_write_tokens
                    )
                } else {
                    String::new()
                };
                if let Some(cost) = usage.cost {
                    total_cost += cost;
                    priced_turns += 1;
                    out.push(format!(
                        "Turn {:>3}: {:>7} in + {:>7} out = ${:.6}{cache_note}",
                        i + 1,
                        usage.input_tokens,
                        usage.output_tokens,
                        cost
                    ));
                } else {
                    if pricing_category == io_runtime::ProviderPricingCategory::Priced
                        || pricing_category == io_runtime::ProviderPricingCategory::ModelNotInTable
                    {
                        missing_cost_turns += 1;
                    }
                    out.push(format!(
                        "Turn {:>3}: {:>7} in + {:>7} out  ({}){cache_note}",
                        i + 1,
                        usage.input_tokens,
                        usage.output_tokens,
                        no_cost_label
                    ));
                }
            }
        }
    }

    out.push(String::new());
    out.push("--- Totals ---".to_string());
    out.push(format!("Input tokens:  {total_input_tokens}"));
    out.push(format!("Output tokens: {total_output_tokens}"));
    if total_cache_read_tokens > 0 || total_cache_write_tokens > 0 {
        out.push(format!(
            "Cache read:    {total_cache_read_tokens} (discounted vs. input)"
        ));
        out.push(format!("Cache write:   {total_cache_write_tokens}"));
    }

    match pricing_category {
        io_runtime::ProviderPricingCategory::Free => {
            out.push("Total cost:    $0.00 (self-hosted / no API charges)".to_string());
        }
        io_runtime::ProviderPricingCategory::SubscriptionBased => {
            out.push("Total cost:    n/a (subscription-billed provider)".to_string());
        }
        io_runtime::ProviderPricingCategory::PassThrough => {
            out.push(
                "Total cost:    n/a (proxy provider — check your backend for charges)".to_string(),
            );
        }
        _ => {
            if priced_turns > 0 && missing_cost_turns == 0 {
                out.push(format!("Total cost:    ${total_cost:.6}"));
            } else if priced_turns > 0 {
                out.push(format!(
                    "Total cost:    ${total_cost:.6} (partial — {missing_cost_turns} turn(s) missing pricing)"
                ));
            } else {
                out.push(format!("Total cost:    n/a ({no_cost_label})"));
            }
        }
    }

    out.push(String::new());
    Ok(out)
}
