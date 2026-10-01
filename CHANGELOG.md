# Changelog

We follow [Semantic Versioning](https://semver.org/) and [Keep a Changelog](https://keepachangelog.com/). CLI and GNOME Shell version separately.

<details>
<summary>To see more about versioning, expand this.</summary>

Every version string starts with `v` (required), e.g. `v0.1.4-beta`.

Here the installable surfaces are **CLI** and **GNOME Shell**. Other Option apps swap in their own names the same way — e.g. **Desktop**, **GTK**, **Web** — whatever you actually ship.

| Part | What you install | Git tag |
| --- | --- | --- |
| **CLI** | `usagenometer` / `usg` in the terminal | `cli/v0.1.4-beta` |
| **GNOME Shell** | the top-bar extension, a thin client over `usg json` | `gnome/v0.1.3-beta` |

A new CLI does not always mean a new GNOME Shell build, and the other way around. The `cli/v*` tags publish to crates.io, the AUR and GitHub Releases; `gnome/v*` tags are companion artifacts.

Sometimes one cut ships **both** surfaces. That is a **mixed release**: the heading gets an `m` before the channel (ex: `v0.1.3m-beta`), git gets the two prefixed tags (`cli/v0.1.3-beta` + `gnome/v0.1.3-beta`), and the notes break out each surface so a small touch on one side does not look equal to a large cut on the other.

Each release heading is the version and date (`## v0.1.3m-beta · 01/08/2026`); under it, a short summary ends with a plain sentence like: “This version was made for both GNOME Shell and CLI with a beta release channel on 01/08/2026 (v0.1.3m-beta).”

### What the channel suffix means

| Suffix | In plain words |
| --- | --- |
| **-alpha** | Very early. Expect missing pieces and lots of bugs. |
| **-beta** | Mostly there, but still rough. Fine to try; not the “official” install. |
| **-stable** | Ready for daily use. This is what we put on GitHub Releases and the AUR. |

We only call something **stable** when we mean it. While the CLI and its companion are still settling, builds stay **beta**.

Older GNOME Shell builds used plain integer tags (`v3`…`v9`) before the per-surface scheme; `v0.1.0m-beta` was the first cut under it.

</details>

## v0.1.5m-beta · 01/10/2026

Local token ledger across 13 AI coding agents: per-model, per-project and per-day token burn with USD cost, budget gates, a tokscale-style TUI tab and a GNOME Tokens row — alongside the existing quota meters. This version was made for both GNOME Shell and CLI with a beta release channel on 01/10/2026 (v0.1.5m-beta).

### GNOME Shell

- New "Tokens" row in the panel driven by `usg tokens --json`, showing today's token burn; hidden gracefully when the installed `usg` predates the ledger.
- `metadata.json` version → `4`.

### CLI

- `usg tokens` — a local token ledger that scans agent session files into SQLite: period/day/model/project/session breakdowns, `--json`, `--privacy` redaction and incremental rescans with per-event dedup.
- Ledger scanners for 13 providers: Claude Code, Codex, Grok, Gemini, Antigravity, Cursor (state.vscdb), OpenCode, GLM (z.ai via Claude transcripts), OMP, Droid (Factory), Pi, Kimi (kimi-cli + kimi-code) and Devin CLI (`devin/cli/sessions.db` + ATIF transcripts).
- `usg tokens --cost` — per-model USD spend from a built-in pricing table (overridable via config); `usg check --budget-usd` exits non-zero when a period's spend crosses a budget.
- `usg tui` gains a Tokens tab with per-model/project breakdown and a 14-day usage chart.
- New quota meters: GLM via api.z.ai (5-hour / weekly), Kimi via api.kimi.com (5-hour / weekly / monthly + booster, with OAuth token refresh), and Devin Cloud (ACU consumption vs organization cap via the Devin API).
- `usg providers --verbose` reports the extended provider contract (`token_ledger`, `real_quota`) so consumers can distinguish verified meter types.
- `--format prometheus` exports `usagenometer_tokens_total`, `usagenometer_token_events_total` and `usagenometer_tokens_last_scan_unixtime` series for scraping.

## v0.1.4-beta · 11/08/2026

Fast local snapshots, durable quota alerts, and an actionable local runway view. This version was made for CLI with a beta release channel on 11/08/2026 (v0.1.4-beta).

- `cache_ttl` now serves a fresh local snapshot before contacting provider APIs, making `usg -c -q`, prompts, bars, and the GNOME thin client fast by default; stale fallback remains visibly marked after a failed live refresh.
- Alert notifications persist their active state under the XDG cache: systemd timers notify once on a threshold crossing, stop repeating an already-active alert, and send a low-urgency recovery notification once it clears.
- `usg history --runway` turns local snapshots into per-meter used percentage, sample count, estimated exhaustion runway, and next reset when supplied by the provider. Flat or reset-heavy series remain explicitly unestimated.
- `usg tui` now shows the same history-backed runway alongside each selected meter.
- `usg providers --verbose` exposes the provider contract (`quota`, `balance`, `resets`, `history`) so consumers can distinguish verified meter types without inventing data.
- Various other reliability tests and documentation polish

## v0.1.3m-beta · 01/08/2026

GNOME Shell becomes a thin client over `usg json`; the CLI gains ETA alerts, systemd units and statusline / ops docs. This version was made for both GNOME Shell and CLI with a beta release channel on 01/08/2026 (v0.1.3m-beta).

### GNOME Shell

- Extension shells out to `usg` / `usagenometer` (`json`, `test`, `providers`) instead of its own JS HTTP/auth providers — no duplicated provider fetch stack.
- Duplicated provider modules removed (`providers/{codex,cursor,antigravity,cli}`, `usageApi.js`, `codexAuth.js`, `lib/http.js`); the pack list shrinks.
- Prefs copy for Claude / Grok matches CLI quota support and shows the CLI binary path.
- `metadata.json` version → `3`.

### CLI

- `--alert-eta HOURS` / config `alert_eta` — warn when history-based exhaustion ETA is within N hours (works with `--notify` / watch).
- Routing hints include remaining % (e.g. `Codex (8%) low → try Cursor (80%)`).
- Example systemd user units under [`packaging/systemd/`](packaging/systemd/) for periodic check+notify.
- Docs: [statusline integrations](docs/statusline.md), [ops/scripting](docs/ops.md), [adding providers](docs/adding-providers.md).
- CI workflow [`.github/workflows/test.yml`](.github/workflows/test.yml) — `cargo test` + GNOME JS normalizer tests on PRs.

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

### CLI

- Terminal meters for Codex, Cursor, Antigravity, Claude, and Grok — black & white panel inspired by [optionMusic](https://github.com/fireflylabss/optionMusic) (`◈ usagenometer`).
- Binaries `usagenometer` and short alias `usg` (clap help, quiet banner, `--json` / `--pretty`).
- Commands: `status` (`st` / `s`, default), `watch` (`w`), `test` (`t`), `providers` (`ls`), `json` (`j`), `version`.
- Filters `-p` / `--provider`, display mode `--display left|used`.
- Codex: reads `~/.codex/auth.json`, ChatGPT WHAM usage windows (5h / weekly).
- Cursor: reads Cursor `state.vscdb`, Auto + Composer / API / on-demand pools.
- Antigravity: secret store / `~/.gemini` OAuth → Cloud Code quota buckets (Gemini + Claude/GPT).
- Claude: Anthropic OAuth `GET /api/oauth/usage` from `~/.claude/.credentials.json` (or `Claude Code-credentials` keyring); falls back to Antigravity `3p-*` pools when OAuth is absent but Antigravity is logged in.
- Grok: OIDC session from `~/.grok/auth.json` → `cli-chat-proxy.grok.com` `/v1/user` + `/v1/billing` (weekly credits / product % / monthly fallback).
- README leads with the CLI; GNOME install is documented as **beta**.
- Tokens are never written by usagenometer; refresh flows stay with the upstream CLIs (`codex login`, Cursor sign-in, `claude login`, `grok login`, Antigravity).
- Cursor, Antigravity, Claude OAuth, and Grok billing surfaces are unofficial/private — degrade per-provider when they change.

### GNOME Shell

- Multi-provider top-bar meters + prefs connection tests.
- Claude / Grok provider modules aligned with CLI OAuth / billing sources.
- Extension metadata / panel label mark the GNOME UI as beta.
