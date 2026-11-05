# ◈ usagenometer

**usagenometer** — AI usage meters in the terminal.  
Short alias: **`usg`**. Reads local auth (Codex CLI, Cursor, Antigravity, Claude, Grok, GLM, Kimi, fx, Command Code, Devin) and prints compact black & white quotas using provider-native credentials. Command Code also contributes token usage from its local transcripts.

```
◈  usagenometer

  Codex  ·  you@example.com  ·  plus
    5 hour usage limit ━━━━━━━━━━────────  42%  ·  reset 3h12m  ·  eta ~2h10m
    Weekly usage limit ━━━────────────────  18%  ·  reset 4d2h

  Cursor  ·  pro
    Auto + Composer    ━━━━━━━━━━━━━─────  74%
    API pool           ━━━━━━────────────  37%
```

## Install

```bash
# crates.io
cargo install usagenometer

# AUR
yay -S usagenometer
# or
paru -S usagenometer

# from source
cargo install --path . --locked --force
# binaries: usagenometer · usg
```

If `command -v usg` points to `~/.local/bin/usg`, update that installation instead of creating a shadowed copy in `~/.cargo/bin`:

```bash
cargo install --path . --locked --force --root "$HOME/.local"
```

| Command | Description |
|---------|-------------|
| `usagenometer` | full name |
| `usg` | short alias |

### Version tags

| Surface | Tag | Ships |
|---------|-----|-------|
| **CLI** | `cli/vX.Y.Z-alpha\|beta\|stable` | crates.io + AUR + GitHub Release |

The GNOME Shell companion shipped through `v0.1.5m-beta` and is discontinued. See [VERSIONING.md](VERSIONING.md), [CHANGELOG.md](CHANGELOG.md), and [packaging/aur/README.md](packaging/aur/README.md).

## Usage

```bash
usg                         # status (default)
usg -c -q                   # compact one-liner (statuslines)
usg status -p codex -p cursor
usg watch --interval 60 --alert 80 --alert-eta 2 --diff
usg check --fail-under 10   # exit 2 if remaining % below threshold
usg check --budget-usd 5 --period day   # exit 2 when today's spend >= $5
usg tokens                  # token ledger totals for today
usg tokens --period week --by model --cost
usg test
usg providers                  # installed agents with locally usable credentials
usg providers --include-unconfigured  # also show installed agents needing credentials
usg providers --only-unconfigured --verbose  # setup commands and credential problems
usg providers --all             # every agent, including agents not installed
usg tokens --all                # include historical usage from unavailable agents
usg doctor                  # auth paths / expiry (no secrets)
usg explain [provider]
usg history --runway          # local burn rate + exhaustion estimate + reset
usg history --spark
usg tokens                    # token ledger: today/7d/30d per provider
usg tokens --by model --by day --period month
usg tokens --since 2026-09-01 -p claude --json
usg config --dump
usg tui                     # interactive live view (quota tab)
usg tui --tokens            # open on the token-usage tab
usg json --pretty
usg --format prometheus
usg completions zsh         # write to stdout
usg --help
```

### Global options

| Flag | Meaning |
|------|---------|
| `-p` / `--provider` | Limit to provider(s); repeatable. Includes `opencode`, `opencode-go`, `opencode-zen`, `omp`, `commandcode`, `droid`, `pi`, `gemini` and `devin-local`. On `tokens` / `check --budget-usd` it filters ledger agents; the config `providers` list does not limit the ledger |
| `--include-unconfigured` | Include installed agents whose credentials need attention |
| `--only-unconfigured` | Show only installed agents whose credentials need attention; `providers --verbose` includes setup commands |
| `--all` | Include agents not installed and agents without usable credentials; on `tokens`, include their recorded history |
| `--display left\|used` | Emphasize remaining (default) or used |
| `-c` / `--compact` | One-liner: `Codex 42% · Cursor 74%` |
| `--privacy` | Redact account emails / identifiers |
| `--alert PCT` | Warn when used % ≥ PCT (also `watch --alert`) |
| `--alert-eta HOURS` | Warn when exhaustion ETA ≤ HOURS (needs history) |
| `--notify` | Desktop `notify-send` on alerts (best-effort) |
| `--json` / `--pretty` | Machine-readable JSON |
| `--format text\|json\|prometheus` | Output format |
| `-q` / `--quiet` | Less stdout noise |

### Commands

| Command | Description |
|---------|-------------|
| `status` (`st`) | Show meters (default). Supports `--compact`. |
| `watch` (`w`) | Refresh on an interval. `--diff` shows only changes. |
| `check` | Exit `0` only when quota verification passes; `1` when unavailable (auth/error, stale fallback, or no percentage quota); `2` when remaining % is below `--fail-under` (default 10) or a token budget is reached. |
| `tokens` (`tok`) | Token ledger totals: `--period today\|week\|month\|all`, `--by model\|project\|session\|day`, `--since`, `--cost` adds a USD column + total. |
| `test` (`t`) | Auth + API connectivity |
| `providers` (`ls`) | Installed, locally authenticated agents; `--verbose` adds credential details and capabilities; `--json` includes installation, credential state and setup command |
| `json` (`j`) | Dump snapshots as JSON |
| `doctor` | Diagnose credential sources, token expiry and refresh availability without network requests |
| `explain` | Inline docs for plan/meter meanings |
| `history` | Local SQLite snapshots (`--runway` for burn/ETA/reset; `--spark` for sparklines) |
| `tokens` | Local token ledger — scans agent session logs (`--period today\|week\|month\|all`, `--by model\|project\|session\|day`, `--since`, `--json`) |
| `config` | Show XDG paths; `--dump` effective TOML |
| `tui` | Two-tab TUI (`tab`/`1`/`2` switch): **Quota** live meters + **Tokens** ledger dashboard — today/7d/30d totals (in/out/cache, UTC days), per-model and per-project breakdowns, last-14-day chart. `j`/`k` select, `r` refresh/rescan, `q` quit. `--tokens` opens on the Tokens tab. |
| `completions` | Generate bash/zsh/fish/… completions to stdout |
| `version` | Print version |

Default status, JSON and token displays exclude agents without a detected executable/native app or usable local credentials. Expired credentials are eligible only where the integration can refresh them; unknown or unreadable native stores stay hidden. Old session files do not prove installation. The remote Devin Cloud integration needs credentials but no local application.

`providers` inspects native credentials locally, without API calls or refresh, and labels JSON with `credential_validation: "local"`. This cannot prove server acceptance or subscription entitlement. Status/JSON/TUI also hide explicit API authentication rejections in default mode; transient API errors remain visible. `usg test PROVIDER` checks the live API even when that provider is hidden. `doctor` remains an unfiltered diagnostic, and `check` keeps failures for providers explicitly selected by `-p` or config instead of silently approving the remaining providers.

The three selection flags are mutually exclusive and override the config provider list; an explicit `-p` still narrows the selection. `providers` always inspects the whole catalog unless you pass `-p`. Token displays and the TUI Tokens tab consider all eligible ledger agents independently of the config quota list. Budget checks continue counting historical spend even after an agent is removed or logged out; use `usg tokens --all --cost` to inspect that spend.

### Config

File: `~/.config/usagenometer/config.toml` (XDG). CLI flags override config.

```toml
providers = []                 # empty = eligible quota/status providers
provider_order = ["codex", "cursor", "claude", "antigravity", "grok"]
watch_interval = 60
display = "left"               # or "used"
alert = 80                     # global used-% threshold
alert_eta = 2                  # hours until exhaustion (optional)
privacy = false
compact = false
notify = false
cache_ttl = 300                # seconds; fresh local snapshot before network fetch
history = true                 # persist snapshots for ETA / history
budget_usd = 5                 # `usg check` token spend gate (optional)
budget_period = "week"         # day | week | month (optional, default day)

[pricing."my-local-model"]     # USD per 1M tokens; overrides embedded table
input = 1.0
output = 2.0
# cache_read / cache_write optional — default to the embedded entry's
# rates when the model is known, else to `input`

[alerts]
codex = 90                     # per-provider used-% overrides
cursor = 85
```

Data / cache:

| Path | Use |
|------|-----|
| `~/.local/share/usagenometer/history.sqlite3` | Snapshot history + token ledger (`token_events`) |
| `~/.cache/usagenometer/snapshots/` | Short cache for stale fallback |

### Statuslines

Compact output for prompts and bars: `usg -c -q`. Ready snippets for Starship, oh-my-posh, fish, bash, and zsh → **[docs/statusline.md](docs/statusline.md)**.

### Scripting & ops

- `usg check --fail-under 10`: exit `2` when remaining &lt; 10% (used &gt; 90%), or `1` when a selected provider's quota cannot be verified. Fresh TTL cache is accepted; stale fallback is not. A confirmed quota or budget breach takes precedence over unavailable data. Diagnostics go to stderr.
- `usg check --budget-usd 5 --period day` — **fail when this day's token cost ≥ $5**. Exit `2`; composes with `--fail-under`. Windows are UTC calendar day / ISO week (Mon) / calendar month.
- `usg tokens --cost --json` — token totals with a `cost_usd` field per row and in `totals`; events on models with no known price are counted in `unpriced_events` and excluded from cost.
- `usg tokens --json`: `groups` contains per-provider rows, with any requested `--by` dimensions; `totals` always contains input, output, cache-read, cache-write and combined token counts. `--cost` adds pricing fields without replacing token totals.
- `usg --format prometheus` — Prometheus text exposition (`usagenometer_used_ratio`, `usagenometer_left_ratio`, `usagenometer_up`).
- `usg json` + `jq` recipes, CI gates, Prometheus scrape, systemd user timers → **[docs/ops.md](docs/ops.md)**.
- Example units: [`packaging/systemd/`](packaging/systemd/).
- Compact mode skips routing hints; quiet/compact skip banners.

### Shell completions

```bash
usg completions bash > ~/.local/share/bash-completion/completions/usg
usg completions zsh  > "${fpath[1]}/_usg"   # or your completions dir
usg completions fish > ~/.config/fish/completions/usg.fish
```

### Providers

| Provider | Source | What you see | Token ledger |
|----------|--------|----------------|--------------|
| **Codex** | `~/.codex/auth.json` + ChatGPT WHAM usage API | 5h / weekly limits | — |
| **Cursor** | CLI `~/.config/cursor/auth.json`, then app `state.vscdb` → `cursor.com/api/usage-summary` | Auto + Composer, API | `state.vscdb` chat bubbles |
| **Antigravity** | agy keyring, then Antigravity app OAuth state, then `~/.gemini/oauth_creds.json` → Cloud Code quota API | Gemini + Claude/GPT pools | `~/.antigravity` + `~/.gemini` sessions |
| **Claude** | `~/.claude/.credentials.json` (or keyring) → Anthropic OAuth usage; else Antigravity `3p-*` | 5h / weekly (+ model buckets) | — |
| **Grok** | `$GROK_HOME/auth.json` or `~/.grok/auth.json` x.ai OIDC session → cli-chat-proxy billing | Weekly credits / products / monthly | `~/.grok` sessions + logs |
| **OpenCode** | — (tokens only) | — | `~/.local/share/opencode` db + `storage/` sessions |
| **OpenCode Go** | Native `opencode-go` key in `opencode.db` or legacy `auth.json` → `/zen/go/v1/usage` | Rolling / weekly / monthly quota windows | Use the shared `opencode` ledger |
| **OpenCode Zen** | Native Console OAuth session in `opencode.db` → `/console/api/billing/status` | Credit balance in USD; available credit for invoiced accounts | Use the shared `opencode` ledger |
| **GLM** | `ZAI_API_KEY` or `env.ANTHROPIC_AUTH_TOKEN` (z.ai `ANTHROPIC_BASE_URL`) → z.ai quota API | 5h / weekly / monthly MCP | `~/.claude/projects` GLM-model records |
| **CommandCode** | `~/.commandcode/auth.json` (`commandcode login`) or `COMMANDCODE_API_KEY` → `api.commandcode.ai` alpha billing endpoints | Credits balance (USD) + 5h / weekly spend windows | `~/.commandcode/projects/*/*.jsonl` assistant usage |
| **OMP** | — (ledger only; `-p omp` points at `usg tokens`) | — | `~/.omp/agent` sessions |
| **Droid** | — | tokens only | `~/.factory/sessions` + `<project>/.factory/sessions` |
| **Pi** | — (ledger only) | — | `~/.pi/agent` sessions |
| **Kimi** | `~/.kimi*/credentials/` OAuth → `api.kimi.com/coding/v1/usages` | 5h / weekly / monthly + booster wallet | `~/.kimi` + `~/.kimi-code` wire.jsonl |
| **Devin (local)** | ledger only (no quota meter) | — | Devin CLI `devin/cli/sessions.db` + `transcripts/*.json` |
| **Devin Cloud** | `DEVIN_API_KEY` → api.devin.ai; or `devin auth login` session → Connect API | ACU today / 7d / cycle vs org cap; CLI session: daily / weekly quota %, credits, overage | — (ACUs, not tokens) |
| **fx** | `VERCEL_OIDC_TOKEN` / `AI_GATEWAY_API_KEY`, else `~/.fx/auth.json` (fx login) or `~/.fx/api-key` → AI Gateway `coding-agent/v1/credits` | Gateway credit balance + total spend (USD) | `~/.fx/usage.jsonl` (Gateway, Codex/Grok subscriptions, custom endpoints; fx keeps 35 days) |

OpenCode Go reads current native credentials from `$XDG_DATA_HOME/opencode/opencode.db` (default `~/.local/share/opencode/opencode.db`) and supports legacy `auth.json` when the native credential store is absent. `OPENCODE_AUTH_CONTENT` overrides Go's stored auth; stored keys take precedence over `OPENCODE_API_KEY`. Go requires an active subscription in the key's organization. See the [Go usage endpoint](https://github.com/anomalyco/opencode/blob/dev/packages/console/app/src/routes/zen/go/v1/usage.ts) for the upstream quota contract.

Zen reads the selected OpenCode Console OAuth credential, not a model API key. Connect with `opencode auth login opencode --method device`, then run `usg test opencode-zen`. Balance requests use the [Console billing API](https://opencode.ai/console/api/billing/status) with bearer authentication and `x-org-id`; microcents are converted to USD without inventing quota percentages or reset times. Native credentials are read-only, and OpenCode owns session refresh. Alternatively, set `OPENCODE_CONSOLE_TOKEN` to a Console access token. If the native credential has no selected organization and the account has several, set `OPENCODE_CONSOLE_ORG_ID`.

The token ledger records local per-agent token usage into `~/.local/share/usagenometer/history.sqlite3`; `usg doctor` shows per-agent scan coverage and event counts. Gemini CLI usage lands under the `gemini` ledger provider while the quota card above stays Antigravity. A provider emits events only when its files carry real token fields — the ledger never fabricates usage.

Each ledger scan commits events, all collector checkpoints and scan-completion metadata in one SQLite transaction. A database failure rolls the scan back and returns an error, so the next scan can retry the unread usage. The TUI reports rescan failures without replacing its last loaded ledger.

Credentials stay in existing provider-native stores. Cursor uses an unexpired CLI login first and falls back to the app's read-only SQLite state when the CLI login is absent, invalid or expired. Antigravity prefers agy and falls back to the system app's read-only OAuth state; refresh uses the native Google OAuth application metadata, with optional `USAGENOMETER_GOOGLE_CLIENT_ID` / `USAGENOMETER_GOOGLE_CLIENT_SECRET` or `AGY_CLIENT_ID` / `AGY_CLIENT_SECRET` overrides.

Grok renews expired x.ai OIDC sessions and atomically updates only the selected entry in its original `auth.json`, including a rotated refresh token when returned. It uses Grok's native lock and preserves unrelated entries and file permissions; there is no separate usagenometer credential store. Cursor and Antigravity credential stores remain read-only. Expired fx logins are left for fx itself to refresh.

Adding a provider is **Rust-only** — see **[docs/adding-providers.md](docs/adding-providers.md)**.

Token costs use an embedded USD-per-1M-token price table (Claude, GPT-5/Codex, Gemini, Grok, Kimi families) with date/vendor-tolerant model matching; unknown models are simply not priced. Override or extend it with `[pricing."<model>"]` in config.

Use `usg providers --verbose` to see the provider contract. A `quota` is a verified usage meter; `balance` is a money/credit meter when the upstream source provides one; `resets` and `history` mean that the snapshot carries a reset window and can feed local runway estimates; `ledger` means a local token-usage scanner ships for the provider. The CLI never manufactures a percentage when a provider only exposes status.

## Tests

```bash
cargo test
```

## Layout

```
src/                  # Rust CLI (usagenometer / usg)
docs/                 # statusline, ops, adding-providers
packaging/systemd/    # user timer examples for alerts
```

## Notes

- Tokens are never written by usagenometer. `usg tokens` reads agent session logs (`~/.claude/projects`, `~/.codex/sessions`) into a local ledger — contents stay on disk, no network calls.
- `cache_ttl` serves a fresh local snapshot before a network request; when a live request fails after that TTL, a recent last-good snapshot may show as `(stale Xm)`.
- Alert notifications are edge-triggered and persisted under the XDG cache, so a systemd timer notifies on threshold crossing and once when the meter recovers.
- Cursor and Antigravity private APIs can change; failures stay per-provider.
- License: **Apache-2.0**.
