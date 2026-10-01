# Ops & scripting recipes

## JSON scripting

```bash
# Pretty snapshots
usg json --pretty

# jq: remaining % for Cursor’s first meter
usg json -q -p cursor | jq '.[0].meters[0].left_percent * 100'

# Fail a script when any enabled provider is not ok
usg json -q | jq -e 'all(.[]; .status == "ok")' >/dev/null

# Token ledger (when supported): today's in/out totals per provider
usg tokens --json | jq '{period, totals, by_provider}'
```

Field names are snake_case (`left_percent`, `reset_at`, `stale_age_secs`). See `src/providers/types.rs`. The GNOME extension reads `usg tokens --json` (or `usg json --tokens`) once per refresh and hides its Tokens row when the command is unavailable.

## CI gate

Exit `2` when any meter’s **remaining** % is below the threshold:

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

```bash
usg check --fail-under 10 --budget-usd 5 --period week
usg tokens --period week --by model --cost   # inspect the spend it gates on
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

Each prometheus export first runs a best-effort ledger scan, so the textfile
doubles as the collector — no separate `usg tokens` run needed.

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
