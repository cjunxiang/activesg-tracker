#!/bin/sh
# Bootstrap a self-hosted ActiveSG collector on a fresh Linux VPS.
#
# Why this exists: Railway blocks outbound UDP, so WARP cannot run there, and
# Railway's own IPs are hard-flagged by ActiveSG's Cloudflare. A small VPS
# (Oracle Always Free works) can run WARP: its egress is consumer-grade, which
# is what the target site actually trusts. The collector pushes results to the
# Railway app, which stays the database + dashboard.
#
# Usage: sudo sh bootstrap.sh
set -eu

INGEST_URL="${INGEST_URL:-https://sg-public-gym-crowd.up.railway.app/ingest}"
INGEST_TOKEN="${INGEST_TOKEN:?set INGEST_TOKEN}"
INTERVAL="${INTERVAL:-600}"
SOCKS="socks5h://127.0.0.1:25344"
SRC='https://activesg.gov.sg/api/trpc/pass.getFacilityCapacities?input=%7B%22json%22%3Anull%2C%22meta%22%3A%7B%22values%22%3A%5B%22undefined%22%5D%7D%7D'

ARCH=$(uname -m)
case "$ARCH" in
  x86_64|amd64) CI_ARCH=x86_64; WP_ARCH=amd64 ;;
  aarch64|arm64) CI_ARCH=aarch64; WP_ARCH=arm64 ;;
  *) echo "unsupported arch: $ARCH" >&2; exit 1 ;;
esac

echo "[1/5] packages"
if command -v apt-get >/dev/null; then
  DEBIAN_FRONTEND=noninteractive apt-get update -qq
  DEBIAN_FRONTEND=noninteractive apt-get install -y -qq ca-certificates curl tar >/dev/null
else
  dnf install -y ca-certificates curl tar >/dev/null 2>&1 || yum install -y ca-certificates curl tar >/dev/null
fi

echo "[2/5] curl-impersonate (browser TLS, needed for the target's bot check)"
curl -fsSL -o /tmp/ci.tgz \
  "https://github.com/lexiforest/curl-impersonate/releases/download/v2.2.3/curl-impersonate-v2.2.3.${CI_ARCH}-linux-gnu.tar.gz"
tar xzf /tmp/ci.tgz -C /tmp curl-impersonate
install -m 0755 /tmp/curl-impersonate /usr/local/bin/curl-impersonate

echo "[3/5] wgcf + wireproxy (WARP client, userspace SOCKS5)"
curl -fsSL -o /usr/local/bin/wgcf \
  "https://github.com/ViRb3/wgcf/releases/download/v2.3.1/wgcf_2.3.1_linux_${WP_ARCH}"
chmod +x /usr/local/bin/wgcf
curl -fsSL -o /tmp/wp.tgz \
  "https://github.com/windtf/wireproxy/releases/download/v1.1.3/wireproxy_linux_${WP_ARCH}.tar.gz"
tar xzf /tmp/wp.tgz -C /tmp wireproxy
install -m 0755 /tmp/wireproxy /usr/local/bin/wireproxy

mkdir -p /etc/activesg
cd /etc/activesg
[ -f wgcf-account.toml ] || wgcf register --accept-tos
wgcf generate
grep -q '\[Socks5\]' wgcf-profile.conf || \
  printf '\n[Socks5]\nBindAddress = 127.0.0.1:25344\n' >> wgcf-profile.conf

echo "[4/5] services"
cat > /etc/systemd/system/warp-socks.service <<'EOF'
[Unit]
Description=WARP SOCKS5 proxy
After=network-online.target

[Service]
ExecStart=/usr/local/bin/wireproxy -c /etc/activesg/wgcf-profile.conf
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF

cat > /etc/systemd/system/activesg-collector.service <<EOF
[Unit]
Description=ActiveSG crowd collector
After=warp-socks.service

[Service]
Type=simple
Environment=INGEST_URL=${INGEST_URL}
Environment=INGEST_TOKEN=${INGEST_TOKEN}
Environment=SOCKS=${SOCKS}
Environment=SRC=${SRC}
Environment=INTERVAL=${INTERVAL}
ExecStart=/usr/local/bin/activesg-collect.sh
Restart=always
RestartSec=30

[Install]
WantedBy=multi-user.target
EOF

cat > /usr/local/bin/activesg-collect.sh <<'EOF'
#!/bin/sh
# One snapshot per INTERVAL, pushed through WARP to the Railway ingest endpoint.
while true; do
  body=$(curl-impersonate --impersonate chrome124 --compressed -sS -m 40 \
           --proxy "$SOCKS" "$SRC" 2>/dev/null)
  if printf '%s' "$body" | grep -q '"timestamp"'; then
    code=$(printf '%s' "$body" | curl -sS -m 30 -o /dev/null -w '%{http_code}' \
             -X POST "$INGEST_URL" \
             -H "authorization: Bearer $INGEST_TOKEN" \
             -H 'content-type: application/json' --data-binary @-)
    echo "$(date -u +%FT%TZ) ingest=$code"
  else
    echo "$(date -u +%FT%TZ) fetch failed (blocked or offline)"
  fi
  sleep "$INTERVAL"
done
EOF
chmod +x /usr/local/bin/activesg-collect.sh

systemctl daemon-reload
systemctl enable --now warp-socks.service activesg-collector.service

# Oracle reclaims Always Free instances judged idle over 7 days: CPU p95 < 20%
# AND network < 20% AND memory < 20%. This collector is idle by that measure, so
# break BOTH the memory and CPU halves below. Size the ballast to your shape:
# keep it above 20% of RAM (3 GB on a 6 GB instance, 3 GB on 12 GB is not enough
# on its own - the CPU cycle covers that). Set KEEPALIVE_MB=0 to skip (PAYG/
# other providers don't need any of this).
KEEPALIVE_MB="${KEEPALIVE_MB:-3000}"
if [ "$KEEPALIVE_MB" -gt 0 ]; then
  cat > /etc/systemd/system/idle-ballast.service <<BALLAST
[Unit]
Description=Hold ${KEEPALIVE_MB}MB in RAM so Oracle does not reclaim this instance as idle
After=local-fs.target

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/bin/sh -c 'dd if=/dev/zero of=/dev/shm/ballast bs=1M count=${KEEPALIVE_MB} status=none'
ExecStop=/bin/rm -f /dev/shm/ballast

[Install]
WantedBy=multi-user.target
BALLAST
  systemctl daemon-reload
  systemctl enable --now idle-ballast.service
fi

# Same rule, CPU half: keep p95 CPU utilisation above 20% with a low-priority
# duty cycle (busy 0.35s, idle 0.65s ~= 35% of one core). nice 19 so the
# collector and WARP always win. Set CPU_DUTY_ON=0 to skip.
CPU_DUTY_ON="${CPU_DUTY_ON:-1}"
if [ "$CPU_DUTY_ON" -eq 1 ]; then
  cat > /usr/local/bin/idle-cpu-cycle.sh <<'CPULOOP'
#!/bin/sh
# Hold instance-wide CPU above Oracle's 20% idle threshold at lowest priority.
# One busy process can use at most one core, so the duty cycle is
#   busy = 35% x cores  (capped at 0.9s), sleep = 1 - busy
# giving ~35% of the whole instance on 1-2 OCPU shapes. nice 19 keeps the
# collector and WARP ahead of it.
N=$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 1)
BUSY=$(awk -v n="$N" 'BEGIN{b=0.35*n; if (b>0.9) b=0.9; printf "%.2f", b}')
SLEEP=$(awk -v b="$BUSY" 'BEGIN{printf "%.2f", 1-b}')
awk -v n="$N" -v b="$BUSY" 'BEGIN{
  pct = 100*b/n
  if (pct < 21) printf "WARN: %d cores, %.2fs busy = %.0f%% of instance (<20%%). Shrink the shape to 1 OCPU or run more workers.\n", n, b, pct
}'
while true; do
  timeout "$BUSY" sh -c 'while :; do :; done'
  sleep "$SLEEP"
done
CPULOOP
  chmod +x /usr/local/bin/idle-cpu-cycle.sh
  cat > /etc/systemd/system/idle-cpu.service <<'CPUSVC'
[Unit]
Description=Low-priority CPU duty cycle so Oracle does not reclaim this instance as idle
After=network-online.target

[Service]
ExecStart=/usr/bin/nice -n 19 /usr/local/bin/idle-cpu-cycle.sh
Restart=always
RestartSec=10
CPUWeight=10

[Install]
WantedBy=multi-user.target
CPUSVC
  systemctl daemon-reload
  systemctl enable --now idle-cpu.service
fi

echo "[5/5] done - watch with: journalctl -u activesg-collector -f"
echo "    first line should read ingest=200"
