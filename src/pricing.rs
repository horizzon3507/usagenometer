//! Embedded per-model USD pricing + cost estimation for token events.
//!
//! Rates are USD per 1M tokens (input, output, cache read, cache write) from
//! each provider's public price list. Unknown models return `None` — a price
//! is never invented. `~/.config/usagenometer/config.toml`
//! `[pricing."<model>"]` entries override or extend the embedded table.

use std::collections::HashMap;

use crate::tokens::TokenEvent;

/// USD per 1M tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelPrice {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

/// Partial per-model override from `config.toml` `[pricing."<model>"]`.
/// Fields left unset merge over the embedded entry when one exists.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct ModelPriceOverride {
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
}

// Canonical (lowercase, dots→dashes) model id → (input, output, cache_read, cache_write).
// Sorted per family; matching is exact → longest-prefix → unambiguous supertyprefix.
const TABLE: &[(&str, [f64; 4])] = &[
    // Anthropic Claude — cache write = 1.25x input, cache read = 0.1x input.
    ("claude-opus-4-1", [15.0, 75.0, 1.50, 18.75]),
    ("claude-opus-4-0", [15.0, 75.0, 1.50, 18.75]),
    ("claude-3-opus", [15.0, 75.0, 1.50, 18.75]),
    ("claude-sonnet-4-5", [3.0, 15.0, 0.30, 3.75]),
    ("claude-sonnet-4-0", [3.0, 15.0, 0.30, 3.75]),
    ("claude-3-7-sonnet", [3.0, 15.0, 0.30, 3.75]),
    ("claude-3-5-sonnet", [3.0, 15.0, 0.30, 3.75]),
    ("claude-haiku-4-5", [1.0, 5.0, 0.10, 1.25]),
    ("claude-3-5-haiku", [0.80, 4.0, 0.08, 1.0]),
    ("claude-3-haiku", [0.25, 1.25, 0.03, 0.30]),
    // OpenAI GPT-5 / Codex — no cache-write premium, cache_read = cached-input rate.
    ("gpt-5-1-codex-max", [1.25, 10.0, 0.125, 1.25]),
    ("gpt-5-1-codex-mini", [0.25, 2.0, 0.025, 0.25]),
    ("gpt-5-1-codex", [1.25, 10.0, 0.125, 1.25]),
    ("gpt-5-1", [1.25, 10.0, 0.125, 1.25]),
    ("gpt-5-codex", [1.25, 10.0, 0.125, 1.25]),
    ("gpt-5-mini", [0.25, 2.0, 0.025, 0.25]),
    ("gpt-5-nano", [0.05, 0.40, 0.005, 0.05]),
    ("gpt-5-pro", [15.0, 120.0, 15.0, 15.0]),
    ("gpt-5", [1.25, 10.0, 0.125, 1.25]),
    ("codex-mini", [1.50, 6.0, 0.375, 1.50]),
    ("gpt-4-1-nano", [0.10, 0.40, 0.025, 0.10]),
    ("gpt-4-1-mini", [0.40, 1.60, 0.10, 0.40]),
    ("gpt-4-1", [2.0, 8.0, 0.50, 2.0]),
    ("gpt-4o-mini", [0.15, 0.60, 0.075, 0.15]),
    ("gpt-4o", [2.50, 10.0, 1.25, 2.50]),
    ("o4-mini", [1.10, 4.40, 0.275, 1.10]),
    ("o3", [2.0, 8.0, 0.50, 2.0]),
    // Google Gemini (gemini-2-5-pro uses the <=200k prompt tier).
    ("gemini-3-pro", [2.0, 12.0, 0.20, 2.0]),
    ("gemini-2-5-pro", [1.25, 10.0, 0.31, 1.25]),
    ("gemini-2-5-flash-lite", [0.10, 0.40, 0.025, 0.10]),
    ("gemini-2-5-flash", [0.30, 2.50, 0.075, 0.30]),
    ("gemini-2-0-flash-lite", [0.075, 0.30, 0.01875, 0.075]),
    ("gemini-2-0-flash", [0.10, 0.40, 0.025, 0.10]),
    // xAI Grok — cache write = input, cache read = discounted cached-input rate.
    ("grok-code-fast-1", [0.20, 1.50, 0.02, 0.20]),
    ("grok-4-1-fast", [0.20, 0.50, 0.05, 0.20]),
    ("grok-4-fast", [0.20, 0.50, 0.05, 0.20]),
    ("grok-4-1", [3.0, 15.0, 0.75, 3.0]),
    ("grok-4", [3.0, 15.0, 0.75, 3.0]),
    ("grok-3-mini", [0.30, 0.50, 0.075, 0.30]),
    ("grok-3", [3.0, 15.0, 0.75, 3.0]),
    // Moonshot Kimi — cache write = input, cache read = cached-input rate.
    // kimi-latest is tiered by context (128k tier); kimi-for-coding / kimi-code
    // are subscription aliases billed like the kimi-k2 family.
    ("kimi-latest", [2.00, 5.00, 0.15, 2.00]),
    ("kimi-k2", [0.60, 2.50, 0.15, 0.60]),
    ("kimi-for-coding", [0.60, 2.50, 0.15, 0.60]),
    ("kimi-code", [0.60, 2.50, 0.15, 0.60]),
];

/// Vendor / region prefixes seen in front of model ids (litellm, Bedrock, Vertex).
const PREFIX_SEGMENTS: &[&str] = &[
    "us",
    "eu",
    "ap",
    "asia",
    "global",
    "anthropic",
    "amazon",
    "bedrock",
    "google",
    "vertex",
    "openai",
    "azure",
    "xai",
    "x-ai",
    "meta",
    "mistral",
    "models",
];

/// Canonical form: lowercase, dashes for `.`/`_`, no vendor prefix, no
/// date/version/`@` suffixes. `claude-sonnet-4-5-20250929` → `claude-sonnet-4-5`.
pub fn normalize_model(model: &str) -> String {
    let mut s = model.trim().to_ascii_lowercase();
    if let Some(pos) = s.rfind('/') {
        s = s[pos + 1..].to_string();
    }
    // Bedrock/Vertex style: us.anthropic.claude-... → claude-...
    loop {
        let Some(pos) = s.find('.') else { break };
        let seg = &s[..pos];
        if PREFIX_SEGMENTS.contains(&seg) {
            s = s[pos + 1..].to_string();
        } else {
            break;
        }
    }
    if let Some(pos) = s.find('@') {
        s.truncate(pos);
    }
    let mut s: String = s
        .chars()
        .map(|c| if c == '.' || c == '_' { '-' } else { c })
        .collect();
    // Strip trailing -<8-digit date>, -<yyyy-mm-dd>, -v<n>, -latest/-preview…
    loop {
        let stripped = s.clone();
        if let Some(rest) = strip_date_suffix(&s) {
            s = rest;
        }
        for suf in ["-latest", "-preview", "-beta", "-exp", "-instruct"] {
            if let Some(rest) = s.strip_suffix(suf) {
                s = rest.to_string();
            }
        }
        if let Some(rest) = strip_v_suffix(&s) {
            s = rest;
        }
        if s == stripped {
            break;
        }
    }
    s
}

fn strip_date_suffix(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let is_digits = |b: &[u8]| !b.is_empty() && b.iter().all(u8::is_ascii_digit);
    // -yyyymmdd
    if s.len() > 9 && bytes[s.len() - 9] == b'-' && is_digits(&bytes[s.len() - 8..]) {
        return Some(s[..s.len() - 9].to_string());
    }
    // -yyyy-mm-dd
    if s.len() > 11 && bytes[s.len() - 11] == b'-' {
        let tail = &s[s.len() - 10..];
        if tail.len() == 10
            && tail.as_bytes()[4] == b'-'
            && tail.as_bytes()[7] == b'-'
            && is_digits(&tail.as_bytes()[..4])
            && is_digits(&tail.as_bytes()[5..7])
            && is_digits(&tail.as_bytes()[8..])
        {
            return Some(s[..s.len() - 11].to_string());
        }
    }
    None
}

fn strip_v_suffix(s: &str) -> Option<String> {
    // trailing -v<digits>
    let rest = s.rsplit_once('-')?.1;
    if rest.len() >= 2 && rest.starts_with('v') && rest[1..].bytes().all(|b| b.is_ascii_digit()) {
        return Some(s[..s.len() - rest.len() - 1].to_string());
    }
    None
}

fn is_boundary(b: u8) -> bool {
    matches!(b, b'-' | b'.' | b':' | b'/' | b'_' | b'@' | b' ')
}

/// Embedded-table price for a model id. `None` when the model is unknown.
pub fn price_for(model: &str) -> Option<ModelPrice> {
    lookup(&normalize_model(model))
}

fn lookup(norm: &str) -> Option<ModelPrice> {
    // 1. exact
    for (key, p) in TABLE {
        if *key == norm {
            return Some(ModelPrice {
                input: p[0],
                output: p[1],
                cache_read: p[2],
                cache_write: p[3],
            });
        }
    }
    // 2. longest table key that prefixes the normalized id at a boundary
    let mut best: Option<(&str, &[f64; 4])> = None;
    for (key, p) in TABLE {
        if norm.len() > key.len()
            && norm.starts_with(key)
            && is_boundary(norm.as_bytes()[key.len()])
            && best.is_none_or(|(b, _)| key.len() > b.len())
        {
            best = Some((key, p));
        }
    }
    if let Some((_, p)) = best {
        return Some(ModelPrice {
            input: p[0],
            output: p[1],
            cache_read: p[2],
            cache_write: p[3],
        });
    }
    // 3. the id is a strict prefix of table keys — only when every candidate
    //    shares one price (e.g. `claude-sonnet-4` → 4-0 and 4-5, same rates).
    let mut shared: Option<[f64; 4]> = None;
    for (key, p) in TABLE {
        if key.len() > norm.len()
            && key.starts_with(norm)
            && is_boundary(key.as_bytes()[norm.len()])
        {
            match shared {
                None => shared = Some(*p),
                Some(prev) if prev == *p => {}
                Some(_) => return None,
            }
        }
    }
    shared.map(|p| ModelPrice {
        input: p[0],
        output: p[1],
        cache_read: p[2],
        cache_write: p[3],
    })
}

/// Price after applying `config.toml` `[pricing."<model>"]` overrides.
/// An override for an unknown model needs `input` + `output` at minimum;
/// unset cache rates default to the input rate (no premium/discount).
pub fn price_for_merged(
    model: &str,
    overrides: &HashMap<String, ModelPriceOverride>,
) -> Option<ModelPrice> {
    let norm = normalize_model(model);
    let base = lookup(&norm);
    let o = overrides
        .iter()
        .find(|(k, _)| normalize_model(k) == norm)
        .map(|(_, v)| *v)
        .or_else(|| overrides.get(model).copied());
    let Some(o) = o else { return base };
    match base {
        Some(mut p) => {
            if let Some(v) = o.input {
                p.input = v;
            }
            if let Some(v) = o.output {
                p.output = v;
            }
            if let Some(v) = o.cache_read {
                p.cache_read = v;
            }
            if let Some(v) = o.cache_write {
                p.cache_write = v;
            }
            Some(p)
        }
        None => match (o.input, o.output) {
            (Some(input), Some(output)) => Some(ModelPrice {
                input,
                output,
                cache_read: o.cache_read.unwrap_or(input),
                cache_write: o.cache_write.unwrap_or(input),
            }),
            _ => None,
        },
    }
}

/// USD cost of one event. `None` when the model has no known price.
pub fn event_cost_usd(ev: &TokenEvent) -> Option<f64> {
    let price = price_for(ev.model.as_deref()?)?;
    Some(cost_at(&price, ev))
}

/// USD cost of one event, honoring config pricing overrides.
pub fn event_cost_usd_merged(
    ev: &TokenEvent,
    overrides: &HashMap<String, ModelPriceOverride>,
) -> Option<f64> {
    let model = ev.model.as_deref()?;
    let price = price_for_merged(model, overrides)?;
    Some(cost_at(&price, ev))
}

fn cost_at(p: &ModelPrice, ev: &TokenEvent) -> f64 {
    (ev.input_tokens as f64 * p.input
        + ev.output_tokens as f64 * p.output
        + ev.cache_read_tokens as f64 * p.cache_read
        + ev.cache_write_tokens as f64 * p.cache_write)
        / 1_000_000.0
}

/// Sum of `event_cost_usd_merged` over events; returns (total, unpriced count).
pub fn total_cost_usd(
    events: &[TokenEvent],
    overrides: &HashMap<String, ModelPriceOverride>,
) -> (f64, usize) {
    let mut total = 0.0;
    let mut unpriced = 0usize;
    for ev in events {
        match event_cost_usd_merged(ev, overrides) {
            Some(c) => total += c,
            None => unpriced += 1,
        }
    }
    (total, unpriced)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(model: &str, input: u64, output: u64, cr: u64, cw: u64) -> TokenEvent {
        TokenEvent {
            provider: "claude".into(),
            model: Some(model.into()),
            session_id: None,
            project: None,
            ts_unix: 0.0,
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: cr,
            cache_write_tokens: cw,
        }
    }

    #[test]
    fn exact_and_dated_lookup() {
        assert_eq!(price_for("claude-sonnet-4-5").unwrap().input, 3.0);
        assert_eq!(price_for("claude-sonnet-4-5-20250929").unwrap().input, 3.0);
        assert_eq!(price_for("Claude-Opus-4-1-20250805").unwrap().output, 75.0);
        assert_eq!(price_for("claude-3-5-sonnet-20241022").unwrap().input, 3.0);
    }

    #[test]
    fn prefixed_and_dotted_ids() {
        assert_eq!(price_for("anthropic/claude-haiku-4-5").unwrap().input, 1.0);
        assert_eq!(
            price_for("us.anthropic.claude-3-5-sonnet-20241022-v1:0")
                .unwrap()
                .output,
            15.0
        );
        assert_eq!(
            price_for("gemini-2.5-pro-preview-05-06").unwrap().output,
            10.0
        );
        assert_eq!(price_for("gpt-5.1-codex").unwrap().input, 1.25);
        assert_eq!(price_for("grok-4-fast-reasoning").unwrap().input, 0.20);
    }

    #[test]
    fn unknown_returns_none() {
        assert!(price_for("mystery-9000").is_none());
        assert!(price_for("grok").is_none()); // ambiguous superprefix
        assert!(event_cost_usd(&ev("mystery-9000", 1, 1, 0, 0)).is_none());
    }

    #[test]
    fn superprefix_shared_price() {
        // claude-sonnet-4 → 4-0 and 4-5 share one price.
        assert_eq!(price_for("claude-sonnet-4").unwrap().input, 3.0);
    }

    #[test]
    fn cost_math() {
        let cost = event_cost_usd(&ev("claude-sonnet-4-5", 1_000_000, 1_000_000, 0, 0)).unwrap();
        assert!((cost - 18.0).abs() < 1e-9);
        let cached = event_cost_usd(&ev("claude-sonnet-4-5", 0, 0, 1_000_000, 1_000_000)).unwrap();
        assert!((cached - 4.05).abs() < 1e-9);
    }

    #[test]
    fn overrides_merge_over_embedded() {
        let mut overrides = HashMap::new();
        overrides.insert(
            "claude-sonnet-4-5".to_string(),
            ModelPriceOverride {
                input: Some(5.0),
                ..Default::default()
            },
        );
        let p = price_for_merged("claude-sonnet-4-5-20250929", &overrides).unwrap();
        assert_eq!(p.input, 5.0);
        assert_eq!(p.output, 15.0);
        // new model from overrides alone
        overrides.insert(
            "my-local-model".to_string(),
            ModelPriceOverride {
                input: Some(1.0),
                output: Some(2.0),
                ..Default::default()
            },
        );
        let p = price_for_merged("my-local-model", &overrides).unwrap();
        assert_eq!(p.cache_read, 1.0);
        assert_eq!(p.cache_write, 1.0);
        // partial override without base → None (never invent)
        overrides.insert(
            "half-priced".to_string(),
            ModelPriceOverride {
                input: Some(1.0),
                ..Default::default()
            },
        );
        assert!(price_for_merged("half-priced", &overrides).is_none());
    }
}
