//! The `/cost` report: per-turn token usage and API cost for the session.

pub async fn show_cost_summary(agent: &io_runtime::Agent) -> anyhow::Result<()> {
    let store = io_runtime::memory::SessionStore::new()?;
    let session_id = agent.session_id().await;
    let session = store.load_session(session_id)?;

    let provider = &session.metadata.provider;
    let model = &session.metadata.model;
    let pricing_category = io_runtime::get_provider_pricing_category(provider, model);

    let mut total_input_tokens: u32 = 0;
    let mut total_output_tokens: u32 = 0;
    let mut total_cost: f64 = 0.0;
    let mut priced_turns: usize = 0;
    let mut missing_cost_turns: usize = 0;

    println!("\n--- Cost Summary ---");
    println!("Session:  {}", &session_id.to_string()[..8]);
    println!("Provider: {provider}");
    println!("Model:    {model}");
    println!("Turns:    {}", session.turns.len());
    println!();

    if session.turns.is_empty() {
        println!("No turns in this session yet.");
        println!();
        return Ok(());
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
                println!("Turn {:>3}: no token data recorded", i + 1);
            }
            Some(usage) => {
                total_input_tokens += usage.input_tokens;
                total_output_tokens += usage.output_tokens;
                if let Some(cost) = usage.cost {
                    total_cost += cost;
                    priced_turns += 1;
                    println!(
                        "Turn {:>3}: {:>7} in + {:>7} out = ${:.6}",
                        i + 1,
                        usage.input_tokens,
                        usage.output_tokens,
                        cost
                    );
                } else {
                    if pricing_category == io_runtime::ProviderPricingCategory::Priced
                        || pricing_category == io_runtime::ProviderPricingCategory::ModelNotInTable
                    {
                        missing_cost_turns += 1;
                    }
                    println!(
                        "Turn {:>3}: {:>7} in + {:>7} out  ({})",
                        i + 1,
                        usage.input_tokens,
                        usage.output_tokens,
                        no_cost_label
                    );
                }
            }
        }
    }

    println!();
    println!("--- Totals ---");
    println!("Input tokens:  {total_input_tokens}");
    println!("Output tokens: {total_output_tokens}");

    match pricing_category {
        io_runtime::ProviderPricingCategory::Free => {
            println!("Total cost:    $0.00 (self-hosted / no API charges)");
        }
        io_runtime::ProviderPricingCategory::SubscriptionBased => {
            println!("Total cost:    n/a (subscription-billed provider)");
        }
        io_runtime::ProviderPricingCategory::PassThrough => {
            println!("Total cost:    n/a (proxy provider — check your backend for charges)");
        }
        _ => {
            if priced_turns > 0 && missing_cost_turns == 0 {
                println!("Total cost:    ${total_cost:.6}");
            } else if priced_turns > 0 {
                println!(
                    "Total cost:    ${total_cost:.6} (partial — {missing_cost_turns} turn(s) missing pricing)"
                );
            } else {
                println!("Total cost:    n/a ({no_cost_label})");
            }
        }
    }

    println!();
    Ok(())
}
