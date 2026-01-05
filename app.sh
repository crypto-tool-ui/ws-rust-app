#!/bin/bash
set -e

ID=${1:-001}
PORT=${2:-3000}

export INSTANCE_ID=${ID}
export WS_PORT=${PORT}
./ws-tcp-proxy
