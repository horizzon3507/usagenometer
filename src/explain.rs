//! Inline docs: what each provider's meters mean.

use crate::cli::ProviderArg;

pub fn explain(provider: Option<ProviderArg>) -> String {
    match provider {
        None => {
            let mut out = String::from("usagenometer — what the meters mean\n\n");
            for p in ProviderArg::all() {
                out.push_str(&section(*p));
                out.push('\n');
            }
            out.push_str(GENERAL);
            out
        }
        Some(p) => {
            let mut out = section(p);
            out.push('\n');
            out.push_str(GENERAL);
            out
        }
    }
}

fn section(p: ProviderArg) -> String {
    match p {
        ProviderArg::Codex => format!(
            "Codex\n\
             Source: ~/.codex/auth.json → ChatGPT WHAM usage API\n\
             Meters:\n\
               · 5 hour usage limit — rolling short window (typically ~5h). Burns with Codex/ChatGPT heavy use; resets on a sliding/window schedule.\n\
               · Weekly usage limit — weekly budget across the plan. Lower urgency than 5h but caps sustained use.\n\
             Display left = remaining; used = consumed toward the cap.\n"
        ),
        ProviderArg::Cursor => format!(
            "Cursor\n\
             Source: Cursor state.vscdb session → cursor.com usage-summary\n\
             Meters (names vary by plan):\n\
               · Auto + Composer — included premium / auto model pool for the billing period.\n\
               · API pool — API / usage-based allowance when present.\n\
               · On-demand — overage / on-demand spend when enabled.\n\
             Percentages are plan-specific; treat them as remaining budget in that pool.\n"
        ),
        ProviderArg::Antigravity => format!(
            "Antigravity\n\
             Source: Antigravity/Gemini OAuth (secret store or ~/.gemini) → Cloud Code quota API\n\
             Meters: Gemini and third-party (Claude/GPT) quota buckets returned by the quota summary.\n\
             Each bucket is a separate pool; low on one does not always mean low on another.\n\
             Token refresh may need USAGENOMETER_GOOGLE_CLIENT_ID / USAGENOMETER_GOOGLE_CLIENT_SECRET.\n"
        ),
        ProviderArg::Claude => format!(
            "Claude\n\
             Source: ~/.claude/.credentials.json (or keyring) → Anthropic OAuth /api/oauth/usage\n\
             Fallback: Antigravity 3p-* Claude pools when OAuth is absent but Antigravity is logged in.\n\
             Meters:\n\
               · 5h / weekly — Claude Code subscription rate windows (similar idea to Codex windows).\n\
               · Model buckets — per-model or tier limits when the API returns them.\n"
        ),
        ProviderArg::Grok => format!(
            "Grok\n\
             Source: ~/.grok/auth.json → cli-chat-proxy billing\n\
             Meters:\n\
               · Weekly credits / products — credit usage percent and product-level usage.\n\
               · Monthly — fallback monthly allowance when present.\n\
             Run grok login if auth is missing.\n"
        ),
    }
}

const GENERAL: &str = "\
Notes\n\
  · usagenometer only reads local auth and provider quota APIs — it never stores tokens and never calls AI models.\n\
  · Private APIs can change; per-provider errors are expected when auth expires or endpoints move.\n\
  · Use `usg doctor` to see which auth files exist and whether tokens look expired.\n\
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explain_all_nonempty() {
        let s = explain(None);
        assert!(s.contains("Codex"));
        assert!(s.contains("Cursor"));
    }
}
