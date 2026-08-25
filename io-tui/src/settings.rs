//! Interactive on/off picker for session & permission settings (`/config`).
//!
//! Only lists config booleans that actually gate runtime behavior —
//! `session.memory_enabled` is a real config field but nothing currently
//! reads it, so it's deliberately left out rather than shipping a toggle
//! that silently does nothing.

use crate::picker;
use io_runtime::config::Config;

struct Toggle {
    label: &'static str,
    desc: &'static str,
    get: fn(&Config) -> bool,
    set: fn(&mut Config, bool),
}

const TOGGLES: &[Toggle] = &[
    Toggle {
        label: "Auto-compact",
        desc: "Summarize history automatically once usage crosses 80% of the context window",
        get: |c| c.session.auto_compact,
        set: |c, v| c.session.auto_compact = v,
    },
    Toggle {
        label: "Auto-allow network fetch",
        desc: "In agent mode, auto-run read-only curl/wget GETs without prompting",
        get: |c| c.permissions.allow_network_fetch,
        set: |c, v| c.permissions.allow_network_fetch = v,
    },
];

/// Run the picker: selecting an item flips it and persists immediately, then
/// the picker reopens so several settings can be toggled in one visit. Esc/`q`
/// exits. Returns whether anything changed, so the caller can decide whether
/// to reload the active agent (e.g. `auto_compact` is baked into `Agent` at
/// construction, so a running session needs a reload to see the new value).
pub fn run() -> anyhow::Result<bool> {
    let mut changed = false;
    loop {
        let config = Config::load()?;
        let items: Vec<(String, &'static str)> = TOGGLES
            .iter()
            .map(|t| {
                let on = (t.get)(&config);
                (
                    format!("{}  [{}]", t.label, if on { "on" } else { "off" }),
                    t.desc,
                )
            })
            .collect();
        let refs: Vec<(&str, &str)> = items.iter().map(|(l, d)| (l.as_str(), *d)).collect();

        match picker::pick_with_hint(&refs, None) {
            Ok(idx) => {
                let mut config = Config::load()?;
                let toggle = &TOGGLES[idx];
                let new_val = !(toggle.get)(&config);
                (toggle.set)(&mut config, new_val);
                config.save()?;
                changed = true;
            }
            Err(e) if e.is::<picker::Dismissed>() => return Ok(changed),
            Err(e) => return Err(e),
        }
    }
}
