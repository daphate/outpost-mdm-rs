# systemd drop-in overrides

systemd unit fragments that **augment** vendor-packaged services on the
production host (`mdm.secondf8n.tech`). Each lives in a
`<service>.service.d/` directory and merges into the corresponding
upstream unit at load time — no need to edit `/usr/lib/systemd/system/*`
(which gets clobbered on package updates).

## Contents

| Drop-in | Target service | Purpose |
|---|---|---|
| `prometheus.service.d/tailscale-bind.conf`               | `prometheus.service`               | Fix boot-time race: Prometheus binds on tailscale IP `100.68.41.91:9090`, but on reboot starts before `tailscaled` brings up the interface → `bind: cannot assign requested address` → exit 1 → service stays dead (upstream unit has `Restart=on-abnormal` which does not cover exit-code failures). The drop-in adds `After=tailscaled.service` and switches to `Restart=on-failure` with retry burst. |
| `prometheus-node-exporter.service.d/tailscale-bind.conf` | `prometheus-node-exporter.service` | Same race for node-exporter on `100.68.41.91:9100`. |

When Prometheus is dead, Grafana dashboards (Fleet overview + Device
drill-down) show **"No data"** because the Prometheus datasource returns
`connection refused`. The visible symptom is identical to a config or
auth issue — the actual root cause is the bind race. See
[`docs/DEPLOY.md` → Observability stack](../../docs/DEPLOY.md#observability-stack-grafana--prometheus--node-exporter)
for the full incident write-up.

## Install (first-time, on the host)

```bash
# As root on mdm.secondf8n.tech:
sudo mkdir -p /etc/systemd/system/prometheus.service.d
sudo mkdir -p /etc/systemd/system/prometheus-node-exporter.service.d

# scp the two .conf files from this repo:
sudo cp /tmp/prometheus-tailscale-bind.conf \
        /etc/systemd/system/prometheus.service.d/tailscale-bind.conf
sudo cp /tmp/node-exporter-tailscale-bind.conf \
        /etc/systemd/system/prometheus-node-exporter.service.d/tailscale-bind.conf

sudo systemctl daemon-reload
sudo systemctl reset-failed prometheus prometheus-node-exporter
sudo systemctl restart prometheus prometheus-node-exporter
```

## Verify

```bash
# Both services should show "Drop-In:" line pointing at /etc/systemd/...
sudo systemctl status prometheus prometheus-node-exporter --no-pager | head -20

# Listeners should be up on tailscale IP:
sudo ss -tlnp | grep -E ':(9090|9100)'

# Prometheus should consider all targets healthy:
curl -s http://100.68.41.91:9090/api/v1/targets \
  | python3 -m json.tool \
  | grep -E '"(job|health|lastError)"'
```

After a reboot, give tailscaled ~30 s to bring up the interface. If the
override is in place, both services will retry every 5 s and bind as
soon as the IP is assigned. Without the override, Prometheus fails once
and stays dead until manually restarted.
