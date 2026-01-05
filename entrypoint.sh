#!/bin/bash
set -e

# start xmrig-proxy in background
/app/xmrig-proxy -c /app/config.json &
XMR_PID=$!

trap "kill -TERM $XMR_PID; wait" SIGINT SIGTERM

# start rust websocket (foreground)
/app/ws-tcp-proxy
