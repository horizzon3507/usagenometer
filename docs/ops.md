# Ops & scripting recipes

## JSON scripting

```bash
# Pretty snapshots
usg json --pretty

# jq: remaining % for Cursor’s first meter
usg json -q -p cursor | jq '.[0].meters[0].left_percent * 100'

# Fail a script when any enabled provider is not ok
usg json -q | jq -e 'all(.[]; .status == "ok")' >/dev/null

# Today's token totals and per-provider rows
usg tokens --period today --json | jq '{period, totals, groups}'
```

Field names are snake_case (`left_percent`, `reset_at`, `stale_age_secs`). See `src/providers/types.rs` for quota snapshots. Token JSON contains `period`, `scanned_new_events`, `groups` and `totals`. Both rows and totals contain `input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_write_tokens` and `total_tokens`; `--cost` adds `cost_usd` and `unpriced_events`. `--by` adds dimension fields to group rows.

## Installed agents and credential setup

```bash
usg providers                                  # installed, locally usable credentials
usg providers --include-unconfigured            # also include installed agents needing login
usg providers --only-unconfigured --verbose     # credential problems and setup commands
usg providers --all --json                      # entire catalog, including uninstalled agents
usg tokens --all --period week --cost           # historical spend from all ledger agents
usg test opencode-go                            # live auth, API and subscription check
```

The listing does not contact APIs or refresh credentials. JSON distinguishes `ready`, `missing`, `expired`, `invalid` and `unknown` credentials and declares `credential_validation: "local"`. Executables/native applications prove installation; session files do not. Remote Devin Cloud has no local installation requirement.

Status, JSON, watch and token displays default to installed agents with locally usable or refreshable credentials. Default displays hide explicit API authentication rejections, but retain transient errors. Unknown credentials require inspection rather than being assumed valid. `--all`, `--include-unconfigured` and `--only-unconfigured` are mutually exclusive; they override the config quota list, while an explicit `-p` still narrows the selection. `providers` uses the full catalog rather than the config quota list.

`doctor` remains diagnostic. `test PROVIDER` runs even for a hidden agent. `check` retains authentication/unavailability failures for providers explicitly selected in CLI/config; display filtering cannot silently approve the remaining providers. To inspect all known quota/status providers, use `--all` with the desired `-p` filters.

OpenCode Go and Zen use separate credentials. Go reads the native `opencode-go` API key and reports real quota windows; subscription entitlement is required. Zen reads the selected `opencode` Console OAuth credential from the native read-only `opencode.db` and fetches `/console/api/billing/status` with `x-org-id`. Run `opencode auth login opencode --method device`, then `usg test opencode-zen`. A model API key alone does not authorize billing. `OPENCODE_CONSOLE_TOKEN` provides an explicit Console access token; `OPENCODE_CONSOLE_ORG_ID` selects an organization if needed. OpenCode owns OAuth refresh. Local OpenCode token totals remain under `-p opencode`, not invented plan-specific totals.

## Quickshell rice integration

The local Hyprland rice at `~/.config/quickshell-cc` opens a compact AI Usage popup from the `IA` top-bar button. Its `usage-agents.json` catalog is limited to Codex, Antigravity and Devin CLI. Their quotas and daily token summaries appear together in one scrollable panel, without an agent selector or page navigation. Only installed agents with usable native credentials appear. This rice-specific selection does not restrict the CLI's full provider catalog.

`AiUsage.qml` derives directly from `UsageApplet`, exposing the `toggle()` method called by `shell.qml` when StatusBar emits `usageRequested`. Verify that top-bar click path, not only the popup's IPC handlers: IPC can work even when a wrapper hides the method expected by the bar.

Both the bar and popup consume `~/.local/bin/usg-usage-json --applet`, an atomic `{agents, snapshots}` envelope. The helper intersects the catalog with `usg --privacy providers --json` (`installed && ready`), queries eligible quota providers together, merges eligible native Devin usage, and excludes API authentication rejections. Transient quota errors remain visible instead of being treated as logout. `--all` returns the eligible snapshot array; no arguments retain the configured quota selection. The helper and token command use `~/.cargo/bin/usg`, installed with `cargo install --path . --locked --force`; do not leave older manually copied binaries ahead of Cargo in PATH.

The adapter converts usg's fractional `percent` and `left_percent` fields to display percentages. Native Devin's `percent` is already on a 0–100 scale; only its fractional `left_percent` is converted. Do not infer the scale from a value's magnitude. Credit balances and spend retain their units and do not become percentage bars.

The popup reads `usg --privacy tokens -p codex -p antigravity -p devin-local --period today --cost --json` and matches `groups` by `provider`. Every eligible agent's quota rows are labeled with its name, followed by its token summary on the same page. “Today” is a UTC window. Missing amounts and empty ledgers are not fabricated zeros; transient failures retain explicitly labeled cached values. Cost is labeled partial when `unpriced_events` is nonzero.

`Escape` or Close hides the popup. Refresh fetches quotas and tokens together and remains disabled until both finish. Automatic refresh runs every 120 seconds only while open. Tab navigation and visible focus outlines cover the actions. The top-bar badge shows only `IA` and the lowest available remaining percentage among these three agents. IPC is also available:

```bash
qs ipc -p "$HOME/.config/quickshell-cc" call ai-usage pages
qs ipc -p "$HOME/.config/quickshell-cc" call ai-usage refresh
qs ipc -p "$HOME/.config/quickshell-cc" call ai-usage toggle
```

## CI gate

`check` exits `0` only when all selected providers supply fresh, usable percentage quotas at or above the remaining threshold. Exit `1` means quota verification is unavailable: authentication/API failure, stale fallback, or no usable percentage quota. Fresh TTL cache is accepted. Exit `2` means a verified quota is below the threshold or a token budget is reached; a breach takes precedence over unavailable data. Diagnostics go to stderr, leaving JSON and Prometheus stdout machine-readable.

```yaml
# GitHub Actions example
- name: AI quota gate
  run: |
    cargo install usagenometer --locked
    usg check --fail-under 15
```

Combine with filters: `usg check --fail-under 10 -p codex -p cursor`.

### Spend budget gate

`usg check --budget-usd N --period day|week|month` also exits `2` when the
token ledger's spend in that UTC window reaches `N`. Both gates run (either can
fail). Windows: calendar day, ISO week (Mon), calendar month. Defaults can come
from `budget_usd` / `budget_period` in `config.toml`; CLI flags win.

Budget gates keep counting stored spend after an agent is uninstalled or its credentials disappear. Display-selection flags do not erase costs; only an explicit `-p` narrows the ledger budget.

```bash
usg check --fail-under 10 --budget-usd 5 --period week
usg tokens --all --period week --by model --cost   # inspect the complete spend it gates on
```

## Prometheus

```bash
usg --format prometheus
```

Metrics:

| Metric | Type | Labels | Source |
|--------|------|--------|--------|
| `usagenometer_up` | gauge | `provider` | quota fetch |
| `usagenometer_used_ratio` | gauge | `provider`, `meter`, `title` | quota fetch |
| `usagenometer_left_ratio` | gauge | `provider`, `meter`, `title` | quota fetch |
| `usagenometer_tokens_total` | counter | `provider`, `model`, `kind` (`input`/`output`/`cache_read`/`cache_write`) | token ledger |
| `usagenometer_token_events_total` | counter | `provider` | token ledger |
| `usagenometer_tokens_last_scan_unixtime` | gauge | — | token ledger |

Each Prometheus export scans the ledger before emitting token metrics, so no
separate `usg tokens` run is needed. A database scan failure returns an error;
events, checkpoints and scan-completion metadata are rolled back together.

Minimal scrape (node_exporter textfile or a tiny exporter):

```yaml
# prometheus.yml fragment
scrape_configs:
  - job_name: usagenometer
    scrape_interval: 5m
    static_configs:
      - targets: ['127.0.0.1:9100']  # whatever serves the textfile dump
```

Example collector loop:

```bash
while true; do
  usg -q --format prometheus > /var/lib/node_exporter/textfile_collector/usagenometer.prom
  sleep 300
done
```

### Daily token budget alert (systemd timer)

Alert when the ledger records more than `USG_TOKEN_BUDGET` tokens in a day.
The script sums today's `usagenometer_tokens_total` increase per provider:

`~/.local/bin/usg-token-budget`:

```bash
#!/usr/bin/env bash
set -euo pipefail
budget="${USG_TOKEN_BUDGET:-2000000}"
day="$(date +%F)"
state="${XDG_CACHE_HOME:-$HOME/.cache}/usagenometer/token-baseline-$day"
now="$(usg -q --format prometheus | awk '
  $1 ~ /^usagenometer_tokens_total/ { sum += $2 } END { print int(sum) }')"
prev="$(cat "$state" 2>/dev/null || echo 0)"
echo "$now" > "$state"
spent=$((now - prev))
if (( spent > budget )); then
  notify-send "usagenometer" "tokens today: $spent > budget $budget"
fi
```

```ini
# ~/.config/systemd/user/usagenometer-tokens.service
[Service]
Type=oneshot
ExecStart=%h/.local/bin/usg-token-budget
```

```ini
# ~/.config/systemd/user/usagenometer-tokens.timer
[Timer]
OnCalendar=*:0/15
Persistent=true

[Install]
WantedBy=timers.target
```

```bash
chmod +x ~/.local/bin/usg-token-budget
systemctl --user daemon-reload
systemctl --user enable --now usagenometer-tokens.timer
```

The baseline file is keyed by date, so each day starts a new comparison and a
missed window backfills on the next fire (`Persistent=true`).

## systemd user timer (alerts)

Unit files live in [`packaging/systemd/`](../packaging/systemd/):

```bash
mkdir -p ~/.config/systemd/user
cp packaging/systemd/usagenometer-alert.service \
   packaging/systemd/usagenometer-alert.timer \
   ~/.config/systemd/user/
# Edit ExecStart if `usg` is not under /usr/bin
systemctl --user daemon-reload
systemctl --user enable --now usagenometer-alert.timer
systemctl --user list-timers | grep usagenometer
```

Oneshoot service runs:

```text
usg -q --alert 80 --alert-eta 2 --notify status
```

- `--alert 80` — used % ≥ 80  
- `--alert-eta 2` — exhaustion ETA ≤ 2 hours (needs history from prior `status`/`watch` runs)  
- `--notify` — `notify-send` when an alert fires  

Alert state is persisted in `~/.cache/usagenometer/alerts.json`: the timer notifies only when a meter crosses into alert state, and sends one low-urgency recovery notification after it clears. Delete that file only when you intentionally want all active alerts treated as new again.

Config equivalents in `~/.config/usagenometer/config.toml`:

```toml
alert = 80
alert_eta = 2
notify = true
history = true
```

## Runway

```bash
usg history --runway
usg history --runway codex
```

Runway is a local linear estimate from recorded snapshots. It reports no estimate for a flat, declining, or reset-heavy series rather than pretending it knows when a quota will run out.

Long-running watch (optional): `usagenometer-watch.service`.
