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
        ProviderArg::Codex => "Codex\n\
             Source: ~/.codex/auth.json → ChatGPT WHAM usage API\n\
             Meters:\n\
               · 5 hour usage limit — rolling short window (typically ~5h). Burns with Codex/ChatGPT heavy use; resets on a sliding/window schedule.\n\
               · Weekly usage limit — weekly budget across the plan. Lower urgency than 5h but caps sustained use.\n\
             Display left = remaining; used = consumed toward the cap.\n".to_string(),
        ProviderArg::Cursor => "Cursor\n\
             Source: CLI cursor/auth.json, then app state.vscdb → cursor.com usage-summary\n\
             Meters (names vary by plan):\n\
               · Auto + Composer — included premium / auto model pool for the billing period.\n\
               · API pool — API / usage-based allowance when present.\n\
               · On-demand — overage / on-demand spend when enabled.\n\
             Percentages are plan-specific; treat them as remaining budget in that pool.\n".to_string(),
        ProviderArg::Antigravity => "Antigravity\n\
             Source: agy keyring, then Antigravity app OAuth state, then ~/.gemini → Cloud Code quota API\n\
             Meters: Gemini and third-party (Claude/GPT) quota buckets returned by the quota summary.\n\
             Each bucket is a separate pool; low on one does not always mean low on another.\n\
             Refresh uses native OAuth metadata; optional USAGENOMETER_GOOGLE_CLIENT_ID / USAGENOMETER_GOOGLE_CLIENT_SECRET or AGY_CLIENT_ID / AGY_CLIENT_SECRET overrides.\n".to_string(),
        ProviderArg::Claude => "Claude\n\
             Source: ~/.claude/.credentials.json (or keyring) → Anthropic OAuth /api/oauth/usage\n\
             Fallback: Antigravity 3p-* Claude pools when OAuth is absent but Antigravity is logged in.\n\
             Meters:\n\
               · 5h / weekly — Claude Code subscription rate windows (similar idea to Codex windows).\n\
               · Model buckets — per-model or tier limits when the API returns them.\n".to_string(),
        ProviderArg::Grok => "Grok\n\
             Source: $GROK_HOME/auth.json or ~/.grok/auth.json x.ai OIDC session → cli-chat-proxy billing\n\
             Meters:\n\
               · Weekly credits / products — credit usage percent and product-level usage.\n\
               · Monthly — fallback monthly allowance when present.\n\
             Expired sessions renew under Grok's native lock; rotated tokens stay in the original auth.json.\n\
             Run grok login if auth is missing or the refresh token is rejected.\n".to_string(),
        ProviderArg::Glm => "GLM\n\
             Source: ZAI_API_KEY or ~/.claude/settings.json env.ANTHROPIC_AUTH_TOKEN\n\
             (used only when ANTHROPIC_BASE_URL is a z.ai host) → z.ai quota API\n\
             Meters:\n\
               · 5h / weekly — GLM Coding Plan usage windows.\n\
               · Monthly · MCP — monthly tool budget when the plan returns it.\n\
             Token ledger: GLM-plan Claude Code sessions under ~/.claude/projects\n\
             are counted as provider glm (the claude scanner skips them).\n".to_string(),
        ProviderArg::Kimi => "Kimi\n\
             Source: ~/.kimi*/credentials/ OAuth → api.kimi.com/coding/v1/usages\n\
             Meters:\n\
               · 5h / weekly / monthly limits — Kimi for Coding subscription windows.\n\
               · Booster wallet — pay-as-you-go balance when the account carries one.\n\
             Token refresh reuses the CLI's own OAuth client and is persisted back.\n\
             Run kimi login if auth is missing.\n".to_string(),
        ProviderArg::DevinCloud => "Devin Cloud\n\
             Source: DEVIN_API_KEY (cog_/apk_) → api.devin.ai, or the devin auth login\n\
             session in ~/.local/share/devin/credentials.toml → Connect API.\n\
             Meters:\n\
               · API key — ACU today / 7d, plus cycle ACU vs the org's monthly cap.\n\
               · CLI session — daily / weekly quota %, prompt credits and overage spend.\n\
             Devin bills in ACUs, not tokens — no token ledger for this provider.\n\
             Set DEVIN_ORG_ID to skip org discovery on non-Enterprise plans.\n".to_string(),
        ProviderArg::Fx => "fx\n\
             Source: VERCEL_OIDC_TOKEN / AI_GATEWAY_API_KEY, else ~/.fx/auth.json (fx login)\n\
             or ~/.fx/api-key → ai-gateway.vercel.sh/coding-agent/v1/credits (same as fx credits).\n\
             Meters:\n\
               · Gateway credits — remaining USD balance and total spend; no cap, so no percent.\n\
             Token ledger: ~/.fx/usage.jsonl, every provider fx routes to (Gateway,\n\
             Codex / Grok subscriptions, custom endpoints). fx keeps 35 days of records.\n\
             Expired logins are left for fx to refresh — run any fx command.\n".to_string(),
        ProviderArg::Omp => "OMP\n\
             Source: none — Oh My Pi exposes no quota API.\n\
             Token ledger: ~/.omp/agent/sessions (OMP_PROFILE / $XDG_DATA_HOME/omp honored).\n\
             Use usg tokens -p omp for usage and cost.\n".to_string(),
        ProviderArg::OpenCodeGo => String::from(
            "OpenCode Go\n\
             Source: OpenCode native opencode-go API key (opencode.db or legacy auth.json) → opencode.ai/zen/go/v1/usage.\n\
             Meters: rolling, weekly and monthly subscription windows with reset timestamps.\n\
             Connect the Go key with opencode auth login; Go entitlement is required.\n",
        ),
        ProviderArg::OpenCodeZen => String::from(
            "OpenCode Zen\n\
             Source: OpenCode Console OAuth session in opencode.db, or OPENCODE_CONSOLE_TOKEN.\n\
             Login: opencode auth login opencode --method device.\n\
             Endpoint: opencode.ai/console/api/billing/status with the selected x-org-id.\n\
             Meters: actual credit balance in USD; available credit for invoiced accounts.\n\
             Model API keys do not authorize billing; no quota percentage or reset is invented.\n\
             Multiple organizations: use the native selection or OPENCODE_CONSOLE_ORG_ID.\n\
             Expired native sessions are left for OpenCode to refresh.\n\
             Local token usage is available via usg tokens -p opencode.\n",
        ),
        ProviderArg::CommandCode => "Command Code\n\
             Source: ~/.commandcode/auth.json (commandcode login) or COMMANDCODE_API_KEY\n\
             → api.commandcode.ai alpha billing endpoints (the same data as the CLI's /usage).\n\
             Meters:\n\
               · Credits — remaining monthly + purchased + free credits in USD; no cap, so no percent.\n\
               · 5 hour / weekly — spend windows {used, cap, resetAt} from the credits endpoint.\n\
             Token ledger: ~/.commandcode/projects/*/*.jsonl (per-message usage).\n\
             Set COMMANDCODE_API_URL to point at a staging or local API.\n".to_string(),
        ProviderArg::OpenCode
        | ProviderArg::Droid
        | ProviderArg::Pi
        | ProviderArg::DevinLocal
        | ProviderArg::Gemini => format!(
            "{}\nLocal token ledger; no quota API. Use usg tokens -p {}.\n",
            crate::providers::provider_label(p.id()),
            p.id()
        ),
    }
}

const GENERAL: &str = "\
Notes\n\
  · usagenometer only reads local auth and provider quota APIs — it never stores tokens and never calls AI models.\n\
  · `usg tokens` builds a local token ledger from agent session logs (Claude Code, Codex, OMP, Kimi, fx, …) — no network.\n\
  · Private APIs can change; per-provider errors are expected when auth expires or endpoints move.\n\
  · Use `usg doctor` to see which auth files exist and whether tokens look expired.\n\
";
