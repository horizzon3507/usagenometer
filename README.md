# Usagenometer

GNOME Shell extension that shows **AI usage meters** in the top bar for multiple providers:

| Provider | Source | What you see |
|----------|--------|----------------|
| **Codex** | Local Codex CLI `~/.codex/auth.json` + ChatGPT usage API | 5h / weekly (and related) limits |
| **Cursor** | Cursor app session (`state.vscdb`) + `cursor.com/api/usage-summary` | **Auto + Composer** and **API** pools |
| **Antigravity** | Secret store OAuth (`service=gemini`, `username=antigravity`) + Cloud Code quota API | Gemini and Claude/GPT 5h + weekly pools |

Compatible with GNOME Shell `45`–`50`.

## Features

- Multi-provider polling with per-provider enable toggles
- Compact panel label (`C 31% · X 42% · A 98%`) or primary-only mode
- Compact provider cards with inline meters and fewer menu actors
- Configurable refresh interval and left/used display
- Preferences connection tests for each provider

## Auth (no secrets stored by the extension)

- **Codex**: reads bearer token from `~/.codex/auth.json` (run `codex login`)
- **Cursor**: reads `cursorAuth/accessToken` from `~/.config/Cursor/User/globalStorage/state.vscdb` (sign in to Cursor)
- **Antigravity**: reads OAuth JSON from the desktop secret store (`secret-tool lookup service gemini username antigravity`), refreshes via Google OAuth when needed (sign in with Antigravity app/CLI)

Claude is currently covered by Antigravity's `Claude/GPT` quota pools. There is no standalone Claude or Grok provider in this extension yet, so they are not exposed as fake settings toggles.

If Antigravity needs to refresh an expired token, provide `USAGENOMETER_GOOGLE_CLIENT_ID` and `USAGENOMETER_GOOGLE_CLIENT_SECRET` in the GNOME Shell environment. Existing access tokens can be used without them.

## Local install

```fish
cd ~/usagenometer

# pack with all sources
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

Then **log out and back in** (Wayland), and:

```fish
gnome-extensions enable usagenometer@horizzon3507
gnome-extensions prefs usagenometer@horizzon3507
```

Dev symlink (optional):

```fish
set EXT ~/.local/share/gnome-shell/extensions/usagenometer@horizzon3507
ln -sfn ~/usagenometer $EXT
glib-compile-schemas $EXT/schemas
```

## Tests

```fish
gjs -m tests/usageApi.test.js
```

## Layout

```
extension.js          # panel + menu
prefs.js              # settings UI
codexAuth.js          # Codex CLI auth
usageApi.js           # Codex ChatGPT usage API
providers/
  registry.js
  types.js
  codex/
  cursor/
  antigravity/
lib/
  http.js
  asyncSubprocess.js
schemas/
```

## Notes

- Tokens are never written to GSettings.
- Cursor and Antigravity private APIs can change; failures surface as provider-level errors without crashing other providers.
- Nested GNOME Shell is unreliable on Wayland GNOME 50; prefer logout/login after install.
