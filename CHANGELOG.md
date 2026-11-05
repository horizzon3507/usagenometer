# Changelog

We follow [Semantic Versioning](https://semver.org/) and [Keep a Changelog](https://keepachangelog.com/). usagenometer is a single CLI surface.

<details>
<summary>To see more about versioning, expand this.</summary>

Every version string starts with `v` (required), e.g. `v0.1.6-beta`, `v0.1.0m-beta`.

Here the installable surface is **CLI** (`usagenometer` / `usg`). Other apps swap in their own names the same way — e.g. **GNOME**, **Desktop**, **Web** — whatever you actually ship.

| Part | What you install | Example |
| --- | --- | --- |
| **CLI** | `usagenometer` / `usg` in the terminal | `v0.1.6-beta` |

With one surface there is no `m` in the tag and no per-surface sections — just the version notes.

Each release heading is the version and date (`## v0.1.6-beta · 04/11/2026`); under it, a short summary ends with a plain sentence like: “This version was made for CLI with a beta release channel on 04/11/2026 (v0.1.6-beta).”

### What the channel suffix means

| Suffix | In plain words |
| --- | --- |
| **-alpha** | Very early. Expect missing pieces and lots of bugs. |
| **-beta** | Mostly there, but still rough. Fine to try; not the “official” install. |
| **-stable** | Ready for daily use. This is what we put on GitHub Releases and the AUR. |

We only call something **stable** when we mean it. While the CLI is still settling, builds stay **beta**.

Versions at `v0.1.5m-beta` and earlier shipped a GNOME Shell companion (tags `gnome/v*`, with an `m` when both surfaces changed). The companion is discontinued from `v0.1.6-beta`; those tags remain historical, and every release from here is CLI-only. Older GNOME Shell builds used plain integer tags (`v3`…`v9`) before the per-surface scheme; `v0.1.0m-beta` was the first cut under it.

</details>

## v0.1.6-beta · 04/11/2026

Command Code joins the quota panel and the token ledger, and the GNOME Shell companion is discontinued. This version was made for CLI with a beta release channel on 04/11/2026 (v0.1.6-beta).

- New `commandcode` ledger provider: scans Command Code transcripts under `~/.commandcode/projects/*/*.jsonl`, counting each assistant message's `inputTokens`, `outputTokens`, `cacheReadTokens` and `cacheWriteTokens` per model, session and project. Editor `.checkpoints.jsonl` sidecars are skipped, and incremental rescans keep per-event dedup.
- New `commandcode` quota meters: remaining credits in USD plus the five-hour and weekly spend windows, read from the same `api.commandcode.ai/alpha` billing endpoints the CLI's `/usage` view uses. `usg explain commandcode` documents them, and `usg doctor` reports the native login and scan coverage.
- `-p commandcode` works for both surfaces: status/JSON fetch credits and windows, while `usg tokens -p commandcode` reads the local transcript ledger. `COMMANDCODE_API_URL` points the meter at a staging or local API.
- **GNOME Shell is discontinued from this release.** The extension sources, preferences UI, gschema, icons, JS bridge and gjs CI job were removed; historical `gnome/v*` tags stay published but receive no new builds.
- New `fx` provider: AI Gateway credit balance and total spend (the same number `fx credits` shows), using fx's credentials in fx's order: `VERCEL_OIDC_TOKEN`, then `AI_GATEWAY_API_KEY`, then `~/.fx/auth.json`, then `~/.fx/api-key`. Expired logins are left for fx to refresh.
- New `fx` ledger scanner over `~/.fx/usage.jsonl`. It covers every backend fx routes to (Gateway, Codex and Grok subscriptions, custom endpoints), splits cache tokens out of fx's cache-inclusive input, and skips pending and incident records.
- `-p omp` and `-p fx` are now accepted. `-p omp` points at `usg tokens`, because OMP has no quota API.
- All catalog agents are selectable with `-p` and config, including OpenCode, Go, Zen, Droid, Pi, Gemini, Devin (local) and CommandCode.
- Default displays now include only installed agents with usable native credentials. `--include-unconfigured` adds installed agents needing credentials, `--only-unconfigured` isolates them with setup commands, and `--all` includes even uninstalled agents. `providers --json` reports credential state and makes its local-only validation explicit.
- Token displays and TUI rescans include every eligible ledger agent independently of the config quota list. `tokens --all` retains access to historical usage; budget checks still count historical spend after uninstall/logout.
- OpenCode Go reads native API keys and real rolling/weekly/monthly quotas from `/zen/go/v1/usage`, including reset times and source-specific 0–100 percentage normalization. Current OpenCode `opencode.db` credentials are supported without reviving logged-out keys from legacy files.
- OpenCode Zen reads real credit balances from `/console/api/billing/status` using the selected Console OAuth session and organization. Signed microcent amounts convert to USD; invoiced accounts also show available credit. Model API keys do not count as Console credentials, expired sessions stay hidden until OpenCode refreshes them, and multiple organizations require an explicit selection.
- Balance-only meters show currency amounts in status, compact output, watch diffs and the TUI instead of unavailable percentages or empty quota bars.
- Droid native v2 credentials are read through the OS secret store or encrypted file fallback using its actual 16-byte AES-GCM nonce. Unknown credential storage is not treated as a verified login.
- Explicit API authentication rejections invalidate cached success snapshots instead of reusing them; default displays hide the rejected agent, while diagnostic commands retain failures.
- `usg doctor` gains fx, OpenCode Go and Zen credential checks; `usg explain` covers every selectable agent.
- Token scans now commit events, collector checkpoints and scan-completion metadata in one SQLite transaction. Database failures return an error and leave usage available for retry; the TUI reports failed rescans instead of claiming zero new events.
- `usg check` now distinguishes verified success (exit `0`), unavailable quota verification (exit `1`) and a quota or budget breach (exit `2`). Authentication/API failures, stale fallback and missing percentage quotas cannot approve a check. Breaches take precedence, and diagnostics go to stderr.
- Token JSON now always includes token counts in `totals`, alongside `groups`; `--cost` adds pricing fields without replacing counts.
- Cursor recognizes CLI logins before falling back to the app's read-only SQLite state, including BLOB-backed auth entries. Usage requests target `cursor.com` directly so cross-host redirects cannot discard the session cookie.
- Antigravity prefers agy credentials and falls back to the system app's native OAuth state. Google refresh uses native app metadata, and Cloud Code quota requests include the account project.
- Grok recognizes native x.ai OIDC sessions and `GROK_HOME`, renews expired sessions under the native credential lock, and atomically preserves rotated refresh tokens in the original auth store.
- `usg doctor` uses the same credential-source resolvers as live usage and reports expiry and refresh availability without network requests.
- The local Hyprland/Quickshell top bar stays limited to installed, credential-ready Codex, Antigravity and Devin CLI, with labeled quotas and daily token summaries in one panel.
- Various other reliability tests and documentation polish

## v0.1.5m-beta · 01/10/2026

Local token ledger across 13 AI coding agents: per-model, per-project and per-day token burn with USD cost, budget gates, a tokscale-style TUI tab and a companion GNOME Tokens row — alongside the existing quota meters. This version was made for both GNOME Shell and CLI with a beta release channel on 01/10/2026 (v0.1.5m-beta).

- GNOME Shell (historical): new "Tokens" row driven by `usg tokens --json`, showing today's token burn and hiding gracefully when the installed `usg` predates the ledger; `metadata.json` version → `4`.
- `usg tokens` — a local token ledger that scans agent session files into SQLite: period/day/model/project/session breakdowns, `--json`, `--privacy` redaction and incremental rescans with per-event dedup.
- Ledger scanners for 13 providers: Claude Code, Codex, Grok, Gemini, Antigravity, Cursor (state.vscdb), OpenCode, GLM (z.ai via Claude transcripts), OMP, Droid (Factory), Pi, Kimi (kimi-cli + kimi-code) and Devin CLI (`devin/cli/sessions.db` + ATIF transcripts).
- `usg tokens --cost` — per-model USD spend from a built-in pricing table (overridable via config); `usg check --budget-usd` exits non-zero when a period's spend crosses a budget.
- `usg tui` gains a Tokens tab with per-model/project breakdown and a 14-day usage chart.
- New quota meters: GLM via api.z.ai (5-hour / weekly), Kimi via api.kimi.com (5-hour / weekly / monthly + booster, with OAuth token refresh), and Devin Cloud (ACU consumption vs organization cap via the Devin API).
- `usg providers --verbose` reports the extended provider contract (`token_ledger`, `real_quota`) so consumers can distinguish verified meter types.
- `--format prometheus` exports `usagenometer_tokens_total`, `usagenometer_token_events_total` and `usagenometer_tokens_last_scan_unixtime` series for scraping.

## v0.1.4-beta · 11/08/2026

Fast local snapshots, durable quota alerts, and an actionable local runway view. This version was made for CLI with a beta release channel on 11/08/2026 (v0.1.4-beta).

- `cache_ttl` now serves a fresh local snapshot before contacting provider APIs, making `usg -c -q`, prompts and bars fast by default; stale fallback remains visibly marked after a failed live refresh.
- Alert notifications persist their active state under the XDG cache: systemd timers notify once on a threshold crossing, stop repeating an already-active alert, and send a low-urgency recovery notification once it clears.
- `usg history --runway` turns local snapshots into per-meter used percentage, sample count, estimated exhaustion runway, and next reset when supplied by the provider. Flat or reset-heavy series remain explicitly unestimated.
- `usg tui` now shows the same history-backed runway alongside each selected meter.
- `usg providers --verbose` exposes the provider contract (`quota`, `balance`, `resets`, `history`) so consumers can distinguish verified meter types without inventing data.
- Various other reliability tests and documentation polish

## v0.1.3m-beta · 01/08/2026

The GNOME Shell companion becomes a thin client over `usg json`; the CLI gains ETA alerts, systemd units and statusline / ops docs. This version was made for both GNOME Shell and CLI with a beta release channel on 01/08/2026 (v0.1.3m-beta).

- GNOME Shell (historical): the extension shells out to `usg` / `usagenometer` (`json`, `test`, `providers`) instead of its own JS HTTP/auth providers — no duplicated provider fetch stack.
- GNOME Shell (historical): duplicated provider modules removed (`providers/{codex,cursor,antigravity,cli}`, `usageApi.js`, `codexAuth.js`, `lib/http.js`); the pack list shrinks.
- GNOME Shell (historical): prefs copy for Claude / Grok matches CLI quota support and shows the CLI binary path; `metadata.json` version → `3`.
- `--alert-eta HOURS` / config `alert_eta` — warn when history-based exhaustion ETA is within N hours (works with `--notify` / watch).
- Routing hints include remaining % (e.g. `Codex (8%) low → try Cursor (80%)`).
- Example systemd user units under [`packaging/systemd/`](packaging/systemd/) for periodic check+notify.
- Docs: [statusline integrations](docs/statusline.md), [ops/scripting](docs/ops.md), [adding providers](docs/adding-providers.md).
- CI workflow [`.github/workflows/test.yml`](.github/workflows/test.yml) — `cargo test` plus the GNOME JS normalizer tests on PRs.

## v0.1.2-beta · 31/07/2026

Apache-2.0 license, VERSIONING.md and the GNOME Shell surface name. This version was made for CLI with a beta release channel on 31/07/2026 (v0.1.2-beta).

- License is **Apache-2.0** (was MIT).
- Versioning docs live in [VERSIONING.md](VERSIONING.md); changelog points there.
- Companion surface renamed to **GNOME Shell** (`gnome/v*`); web surface removed from the scheme.
- Repo/docs references use [optionMusic](https://github.com/fireflylabss/optionMusic) (not optMusic).

## v0.1.1-beta · 31/07/2026

Config, history/ETA, alerts, doctor, TUI and scripting hooks — prefer the CLI over the GNOME panel. This version was made for CLI with a beta release channel on 31/07/2026 (v0.1.1-beta).

- Persistent XDG config (`~/.config/usagenometer/config.toml`) + `usg config [--dump]`; CLI flags override.
- Threshold alerts (`--alert` / config / per-provider `[alerts]`), optional `notify-send`, watch de-dupe.
- Local SQLite history (`usg history [--spark]`), exhaustion ETA on status when enough samples exist.
- Compact one-liner (`-c` / `--compact`) for shell statuslines.
- Short snapshot cache with `(stale Xm)` fallback on API failure.
- `usg doctor` — auth path / expiry / Antigravity OAuth env checks (no secrets).
- `usg explain [provider]` — inline meter/plan docs.
- `usg check --fail-under PCT` — scripting exit code when remaining % is low.
- `--format prometheus` text exposition; existing `--json` / `--pretty` retained.
- `usg watch --diff` — show only meters that changed between polls.
- Routing hint when one provider is low and others have headroom (skipped in compact/quiet).
- Privacy mode (`--privacy` / config) redacts account identifiers in status, watch, history, doctor, JSON.
- Interactive `usg tui` (ratatui; `q` / `r` / `j`/`k`).
- `usg completions <shell>` via clap_complete.
- crates.io + AUR packaging (`packaging/aur/`); tags `cli/v*`.

## v0.1.0m-beta · 28/07/2026

First public beta: multi-provider meters in the terminal plus a GNOME Shell top-bar companion — Claude/Grok private APIs can change. This version was made for both CLI and GNOME Shell with a beta release channel on 28/07/2026 (v0.1.0m-beta).

- Terminal meters for Codex, Cursor, Antigravity, Claude, and Grok — black & white panel inspired by [optionMusic](https://github.com/fireflylabss/optionMusic) (`◈ usagenometer`).
- Binaries `usagenometer` and short alias `usg` (clap help, quiet banner, `--json` / `--pretty`).
- Commands: `status` (`st` / `s`, default), `watch` (`w`), `test` (`t`), `providers` (`ls`), `json` (`j`), `version`.
- Filters `-p` / `--provider`, display mode `--display left|used`.
- Codex: reads `~/.codex/auth.json`, ChatGPT WHAM usage windows (5h / weekly).
- Cursor: reads Cursor `state.vscdb`, Auto + Composer / API / on-demand pools.
- Antigravity: secret store / `~/.gemini` OAuth → Cloud Code quota buckets (Gemini + Claude/GPT).
- Claude: Anthropic OAuth `GET /api/oauth/usage` from `~/.claude/.credentials.json` (or `Claude Code-credentials` keyring); falls back to Antigravity `3p-*` pools when OAuth is absent but Antigravity is logged in.
- Grok: OIDC session from `~/.grok/auth.json` → `cli-chat-proxy.grok.com` `/v1/user` + `/v1/billing` (weekly credits / product % / monthly fallback).
- Tokens are never written by usagenometer; refresh flows stay with the upstream CLIs (`codex login`, Cursor sign-in, `claude login`, `grok login`, Antigravity).
- Cursor, Antigravity, Claude OAuth, and Grok billing surfaces are unofficial/private — degrade per-provider when they change.
- GNOME Shell (historical): multi-provider top-bar meters and prefs connection tests; Claude / Grok modules aligned with the CLI OAuth / billing sources; metadata and panel label marked the UI as beta.
