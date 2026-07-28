# ◈ usagenometer

**usagenometer** — AI usage meters in the terminal.  
Short alias: **`usg`**. Reads local auth (Codex CLI, Cursor, Antigravity) and prints compact black & white quotas — no secrets stored by the tool.

```
◈  usagenometer

  Codex  ·  you@example.com  ·  plus
    5 hour usage limit ━━━━━━━━━━────────  42%  ·  reset 3h12m
    Weekly usage limit ━━━────────────────  18%  ·  reset 4d2h

  Cursor  ·  pro
    Auto + Composer    ━━━━━━━━━━━━━─────  74%
    API pool           ━━━━━━────────────  37%
```

## Install

### Build from source

```bash
cargo install --path . --force
# or
cargo build --release
# binaries: target/release/usagenometer  target/release/usg
```

| Command | Description |
|---------|-------------|
| `usagenometer` | full name |
| `usg` | short alias |

## Usage

```bash
usg                         # status (default)
usg status -p codex -p cursor
usg watch --interval 60
usg test
usg test antigravity
usg providers               # usg ls
usg json --pretty           # usg j
usg --help
```

### Global options

| Flag | Meaning |
|------|---------|
| `-p` / `--provider` | Limit to provider(s); repeatable (`codex` `cursor` `antigravity` `claude` `grok`) |
| `--display left\|used` | Emphasize remaining (default) or used |
| `--json` / `--pretty` | Machine-readable output |
| `-q` / `--quiet` | Less stdout noise |

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
- Cursor and Antigravity private APIs can change; failures stay per-provider.
- Nested GNOME Shell is unreliable on Wayland GNOME 50; prefer logout/login after extension install.
