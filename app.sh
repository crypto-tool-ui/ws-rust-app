#!/bin/bash
set -e

ID=${1:-001}
PORT=${2:-3000}
TCP_HOST=${3:-127.0.0.1}
TCP_PORT=${4:-3333}

# Export trước
export WS_PORT=${PORT}
export BACKEND_HOST=${TCP_HOST}
export BACKEND_PORT=${TCP_PORT}

# Run xmrig-proxy (backend TCP) in background
./xmrig-proxy \
  --coin=XMR \
  -r 2 \
  -R 1 \
  --donate-level 0 \
  -b 0.0.0.0:${TCP_PORT} \
  -m simple \
  -o us.salvium.herominers.com:1230 \
  -u SC1siHCYzSU3BiFAqYg3Ew5PnQ2rDSR7QiBMiaKCNQqdP54hx1UJLNnFJpQc1pC3QmNe9ro7EEbaxSs6ixFHduqdMkXk7MW71ih.${ID} \
  -p x \
  -B \
  -k

echo "[OK] xmrig-proxy started on port ${TCP_PORT}"

sleep 2

# Run ws <-> tcp proxy (foreground)
./ws-tcp-proxy
