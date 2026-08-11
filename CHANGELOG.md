# Changelog

We follow [Semantic Versioning](https://semver.org/) and keep the primary CLI and its GNOME Shell companion as separate installable surfaces.

<details>
<summary>To see more about versioning, expand this.</summary>

Every release heading starts with `v`, for example `v0.1.4-beta`. The primary **CLI** is tagged as `cli/vX.Y.Z-channel` and publishes to crates.io, AUR, and GitHub Releases. The **GNOME Shell** companion uses `gnome/vX.Y.Z-channel` and is versioned separately.

When one cut contains substantial user-facing work for both surfaces, it is recorded as a mixed release with `m` before the channel — for example `v0.1.4m-beta` — and its notes split the CLI and GNOME work. A CLI-only release does not bump GNOME merely for being compatible.

| Suffix | In plain words |
| --- | --- |
| **-alpha** | Very early; expect missing features and breakage. |
| **-beta** | Usable, but still settling. |
| **-stable** | Ready for daily use and deliberately release-ready. |

</details>

## v0.1.4-beta · 11/08/2026

Fast local snapshots, durable quota alerts, and an actionable local runway view. This version was made for CLI with a beta release channel on 11/08/2026 (v0.1.4-beta).

- `cache_ttl` now serves a fresh local snapshot before contacting provider APIs, making `usg -c -q`, prompts, bars, and the GNOME thin client fast by default; stale fallback remains visibly marked after a failed live refresh.
- Alert notifications persist their active state under the XDG cache: systemd timers notify once on a threshold crossing, stop repeating an already-active alert, and send a low-urgency recovery notification once it clears.
- `usg history --runway` turns local snapshots into per-meter used percentage, sample count, estimated exhaustion runway, and next reset when supplied by the provider. Flat or reset-heavy series remain explicitly unestimated.
- `usg tui` now shows the same history-backed runway alongside each selected meter.
- `usg providers --verbose` exposes the provider contract (`quota`, `balance`, `resets`, `history`) so consumers can distinguish verified meter types without inventing data.
- Various other reliability tests and documentation polish

## [CLI 0.1.3-beta] - 2026-08-01

> **Beta** — GNOME thin client over `usg json`; ETA alerts; statusline / ops docs.

### Added

- `--alert-eta HOURS` / config `alert_eta` — warn when history-based exhaustion ETA is within N hours (works with `--notify` / watch).
- Example systemd user units under [`packaging/systemd/`](packaging/systemd/) for periodic check+notify.
- Docs: [statusline integrations](docs/statusline.md), [ops/scripting](docs/ops.md), [adding providers](docs/adding-providers.md).
- CI workflow [`.github/workflows/test.yml`](.github/workflows/test.yml) — `cargo test` + GNOME JS normalizer tests on PRs.

### Changed

- Routing hints include remaining % (e.g. `Codex (8%) low → try Cursor (80%)`).

## [GNOME Shell 0.1.3-beta] - 2026-08-01

> **Beta** companion — thin client over the CLI; no duplicated provider fetch stack.

### Changed

- Extension shells out to `usg` / `usagenometer` (`json`, `test`, `providers`) instead of JS HTTP/auth providers.
- Prefs copy for Claude / Grok matches CLI quota support; shows CLI binary path.
- Pack list shrinks (no `usageApi.js` / per-provider fetch modules).
- `metadata.json` version → `3`.

### Removed

- Duplicated GNOME JS provider fetch/auth modules (`providers/{codex,cursor,antigravity,cli}`, `usageApi.js`, `codexAuth.js`, `lib/http.js`).

## [CLI 0.1.2-beta] - 2026-07-31

> **Beta** — Apache-2.0 license; VERSIONING.md; GNOME Shell surface naming.

### Changed

- License is **Apache-2.0** (was MIT).
- Versioning docs live in [VERSIONING.md](VERSIONING.md); changelog points there.
- Companion surface renamed to **GNOME Shell** (`gnome/v*`); web surface removed from the scheme.
- Repo/docs references use [optionMusic](https://github.com/fireflylabss/optionMusic) (not optMusic).

## [CLI 0.1.1-beta] - 2026-07-31

> **Beta** — config, history/ETA, alerts, doctor, TUI, scripting hooks. Prefer the CLI over the GNOME panel.

### Added

**CLI (Rust)**

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

## [GNOME Shell 0.1.0-beta] - 2026-07-28

> **Beta** companion — GNOME Shell top-bar meters. Prefer the CLI for day-to-day use.

### Added

- Multi-provider top-bar meters + prefs connection tests.
- Claude / Grok provider modules aligned with CLI OAuth / billing sources.
- Extension metadata / panel label mark the GNOME UI as beta.

## [CLI 0.1.0-beta] - 2026-07-28

> **Beta** — multi-provider meters work; Claude/Grok private APIs can change. Prefer the CLI over the GNOME panel.

### Added

**CLI (Rust)**

- Terminal meters for Codex, Cursor, Antigravity, Claude, and Grok — black & white panel inspired by [optionMusic](https://github.com/fireflylabss/optionMusic) (`◈ usagenometer`).
- Binaries `usagenometer` and short alias `usg` (clap help, quiet banner, `--json` / `--pretty`).
- Commands: `status` (`st` / `s`, default), `watch` (`w`), `test` (`t`), `providers` (`ls`), `json` (`j`), `version`.
- Filters `-p` / `--provider`, display mode `--display left|used`.
- Codex: reads `~/.codex/auth.json`, ChatGPT WHAM usage windows (5h / weekly).
- Cursor: reads Cursor `state.vscdb`, Auto + Composer / API / on-demand pools.
- Antigravity: secret store / `~/.gemini` OAuth → Cloud Code quota buckets (Gemini + Claude/GPT).
- Claude: Anthropic OAuth `GET /api/oauth/usage` from `~/.claude/.credentials.json` (or `Claude Code-credentials` keyring); falls back to Antigravity `3p-*` pools when OAuth is absent but Antigravity is logged in.
- Grok: OIDC session from `~/.grok/auth.json` → `cli-chat-proxy.grok.com` `/v1/user` + `/v1/billing` (weekly credits / product % / monthly fallback).

### Changed

- README leads with the CLI; GNOME install is documented as **beta**.

### Notes

- Tokens are never written by usagenometer; refresh flows stay with the upstream CLIs (`codex login`, Cursor sign-in, `claude login`, `grok login`, Antigravity).
- Cursor, Antigravity, Claude OAuth, and Grok billing surfaces are unofficial/private — degrade per-provider when they change.
