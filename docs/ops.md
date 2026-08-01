# Ops & scripting recipes

## JSON scripting

```bash
# Pretty snapshots
usg json --pretty

# jq: remaining % for Cursor’s first meter
usg json -q -p cursor | jq '.[0].meters[0].left_percent * 100'

# Fail a script when any enabled provider is not ok
usg json -q | jq -e 'all(.[]; .status == "ok")' >/dev/null
```

Field names are snake_case (`left_percent`, `reset_at`, `stale_age_secs`). See `src/providers/types.rs`.

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

## Prometheus

```bash
usg --format prometheus
```

Metrics: `usagenometer_up`, `usagenometer_used_ratio`, `usagenometer_left_ratio`
(labels: `provider`, `meter`, `title`).

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

Config equivalents in `~/.config/usagenometer/config.toml`:

```toml
alert = 80
alert_eta = 2
notify = true
history = true
```

Long-running watch (optional): `usagenometer-watch.service`.
