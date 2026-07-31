# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html) with an explicit **release channel** suffix (same scheme as [optMusic](https://github.com/fireflylabss/optMusic)).

### Release channels (`x.y.z-<channel>`)

| Channel | Tag example | Meaning |
|---------|-------------|---------|
| **alpha** | `0.1.0-alpha` | Extremely early. Features incomplete; bugs are expected and common. |
| **beta** | `0.1.0-beta` | Feature set nearly complete, but still rough — bugs and hard edges remain. |
| **stable** | `0.2.0-stable` | Production-ready: finished for that version, few or no known bugs. |

The **CLI** (`usagenometer` / `usg`) is the primary surface. The **GNOME Shell extension** ships in-tree as a **beta** companion — not the default install path.

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

## [CLI 0.1.0-beta] - 2026-07-28

> **Beta** — multi-provider meters work; Claude/Grok private APIs can change. Prefer the CLI over the GNOME panel.

### Added

**CLI (Rust)**

- Terminal meters for Codex, Cursor, Antigravity, Claude, and Grok — black & white panel inspired by optMusic (`◈ usagenometer`).
- Binaries `usagenometer` and short alias `usg` (clap help, quiet banner, `--json` / `--pretty`).
- Commands: `status` (`st` / `s`, default), `watch` (`w`), `test` (`t`), `providers` (`ls`), `json` (`j`), `version`.
- Filters `-p` / `--provider`, display mode `--display left|used`.
- Codex: reads `~/.codex/auth.json`, ChatGPT WHAM usage windows (5h / weekly).
- Cursor: reads Cursor `state.vscdb`, Auto + Composer / API / on-demand pools.
- Antigravity: secret store / `~/.gemini` OAuth → Cloud Code quota buckets (Gemini + Claude/GPT).
- Claude: Anthropic OAuth `GET /api/oauth/usage` from `~/.claude/.credentials.json` (or `Claude Code-credentials` keyring); falls back to Antigravity `3p-*` pools when OAuth is absent but Antigravity is logged in.
- Grok: OIDC session from `~/.grok/auth.json` → `cli-chat-proxy.grok.com` `/v1/user` + `/v1/billing` (weekly credits / product % / monthly fallback).

**GNOME Shell extension (beta)**

- Multi-provider top-bar meters + prefs connection tests (existing panel work).
- Claude / Grok provider modules updated to the same OAuth / billing sources as the CLI.

### Changed

- README leads with the CLI; GNOME install is documented as **beta**.
- Extension metadata / panel label mark the GNOME UI as beta.

### Notes

- Tokens are never written by usagenometer; refresh flows stay with the upstream CLIs (`codex login`, Cursor sign-in, `claude login`, `grok login`, Antigravity).
- Cursor, Antigravity, Claude OAuth, and Grok billing surfaces are unofficial/private — degrade per-provider when they change.
