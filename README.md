# ◈ usagenometer

**usagenometer** — AI usage meters in the terminal.  
Short alias: **`usg`**. Reads local auth (Codex CLI, Cursor, Antigravity) and prints compact black & white quotas — no secrets stored by the tool.

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
cargo install --path . --force
# binaries: usagenometer · usg
```

| Command | Description |
|---------|-------------|
| `usagenometer` | full name |
| `usg` | short alias |

### Version tags

| Surface | Tag | Ships |
|---------|-----|-------|
| **CLI** | `cli/vX.Y.Z-alpha\|beta\|stable` | crates.io + AUR + GitHub Release |
| **GNOME Shell** | `gnome/vX.Y.Z-…` | GNOME Shell extension (companion) |

See [VERSIONING.md](VERSIONING.md), [CHANGELOG.md](CHANGELOG.md), and [packaging/aur/README.md](packaging/aur/README.md).

## Usage

```bash
usg                         # status (default)
usg -c                      # compact one-liner (statuslines)
usg status -p codex -p cursor
usg watch --interval 60 --alert 80 --diff
usg check --fail-under 10   # exit 2 if remaining % below threshold
usg test
usg doctor                  # auth paths / expiry (no secrets)
usg explain [provider]
usg history --spark
usg config --dump
usg tui                     # interactive live view
usg json --pretty
usg --format prometheus
usg completions zsh         # write to stdout
usg --help
```

### Global options

| Flag | Meaning |
|------|---------|
| `-p` / `--provider` | Limit to provider(s); repeatable (`codex` `cursor` `antigravity` `claude` `grok`) |
| `--display left\|used` | Emphasize remaining (default) or used |
| `-c` / `--compact` | One-liner: `Codex 42% · Cursor 74%` |
| `--privacy` | Redact account emails / identifiers |
| `--alert PCT` | Warn when used % ≥ PCT (also `watch --alert`) |
| `--notify` | Desktop `notify-send` on alerts (best-effort) |
| `--json` / `--pretty` | Machine-readable JSON |
| `--format text\|json\|prometheus` | Output format |
| `-q` / `--quiet` | Less stdout noise |

### Commands

| Command | Description |
|---------|-------------|
| `status` (`st`) | Show meters (default). Supports `--compact`. |
| `watch` (`w`) | Refresh on an interval. `--diff` shows only changes. |
| `check` | Exit `2` if any meter **remaining** % is below `--fail-under` (default 10). |
| `test` (`t`) | Auth + API connectivity |
| `providers` (`ls`) | List providers |
| `json` (`j`) | Dump snapshots as JSON |
| `doctor` | Diagnose auth files, token expiry, Antigravity OAuth env |
| `explain` | Inline docs for plan/meter meanings |
| `history` | Local SQLite snapshots (`--spark` for burn sparklines) |
| `config` | Show XDG paths; `--dump` effective TOML |
| `tui` | Interactive TUI (`q` quit, `r` refresh, `j`/`k` select) |
| `completions` | Generate bash/zsh/fish/… completions to stdout |
| `version` | Print version |

### Config

File: `~/.config/usagenometer/config.toml` (XDG). CLI flags override config.

```toml
providers = []                 # empty = all
provider_order = ["codex", "cursor", "claude", "antigravity", "grok"]
watch_interval = 60
display = "left"               # or "used"
alert = 80                     # global used-% threshold
privacy = false
compact = false
notify = false
cache_ttl = 300                # seconds (stale fallback window scaling)
history = true                 # persist snapshots for ETA / history

[alerts]
codex = 90                     # per-provider used-% overrides
cursor = 85
```

Data / cache:

| Path | Use |
|------|-----|
| `~/.local/share/usagenometer/history.sqlite3` | Snapshot history |
| `~/.cache/usagenometer/snapshots/` | Short cache for stale fallback |

### Scripting notes

- `usg check --fail-under 10` — **fail when remaining &lt; 10%** (used &gt; 90%). Exit `2` on failure.
- `usg --format prometheus` — Prometheus text exposition (`usagenometer_used_ratio`, `usagenometer_left_ratio`, `usagenometer_up`).
- Compact mode skips routing hints; quiet/compact skip banners.

### Shell completions

```bash
usg completions bash > ~/.local/share/bash-completion/completions/usg
usg completions zsh  > "${fpath[1]}/_usg"   # or your completions dir
usg completions fish > ~/.config/fish/completions/usg.fish
```

### Providers

| Provider | Source | What you see |
|----------|--------|----------------|
| **Codex** | `~/.codex/auth.json` + ChatGPT WHAM usage API | 5h / weekly limits |
| **Cursor** | Cursor `state.vscdb` + `cursor.com/api/usage-summary` | Auto + Composer, API |
| **Antigravity** | secret store / `~/.gemini` + Cloud Code quota API | Gemini + Claude/GPT pools |
| **Claude** | `~/.claude/.credentials.json` (or keyring) → Anthropic OAuth usage; else Antigravity `3p-*` | 5h / weekly (+ model buckets) |
| **Grok** | `~/.grok/auth.json` → cli-chat-proxy billing | Weekly credits / products / monthly |

Auth is read-only from existing logins (`codex login`, Cursor sign-in, `claude login`, `grok login`, Antigravity). For Antigravity token refresh, set `USAGENOMETER_GOOGLE_CLIENT_ID` and `USAGENOMETER_GOOGLE_CLIENT_SECRET` when needed.

## GNOME Shell extension (beta)

The top-bar GNOME extension in this repo is **beta**. Prefer the CLI for day-to-day use. Compatible with GNOME Shell `45`–`50`.

```fish
cd ~/usagenometer

gnome-extensions pack --force \
  --extra-source=codexAuth.js \
  --extra-source=constants.js \
  --extra-source=usageApi.js \
  --extra-source=lib \
  --extra-source=providers \
  --extra-source=icons \
  -o /tmp

gnome-extensions install --force /tmp/usagenometer@horizzon3507.shell-extension.zip
glib-compile-schemas ~/.local/share/gnome-shell/extensions/usagenometer@horizzon3507/schemas
```

Then log out/in (Wayland) and:

```fish
gnome-extensions enable usagenometer@horizzon3507
gnome-extensions prefs usagenometer@horizzon3507
```

## Tests

```bash
cargo test
gjs -m tests/usageApi.test.js
```

## Layout

```
src/                  # Rust CLI (usagenometer / usg)
extension.js          # GNOME panel + menu (beta)
prefs.js              # GNOME settings UI (beta)
providers/            # GNOME JS providers (beta)
```

## Notes

- Tokens are never written by usagenometer.
- On API failure, a recent cached snapshot may show as `(stale Xm)` when available.
- Cursor and Antigravity private APIs can change; failures stay per-provider.
- Nested GNOME Shell is unreliable on Wayland GNOME 50; prefer logout/login after extension install.
