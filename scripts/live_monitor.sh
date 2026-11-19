#!/usr/bin/env bash
# Starts the live monitor with Dhan credentials from .env.
#
#   scripts/live_monitor.sh                       # all stocks, until Ctrl-C
#   scripts/live_monitor.sh --show RELIANCE,TCS   # any live-monitor options
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ ! -f .env ]]; then
  echo "missing .env with DHAN_CLIENT_ID, DHAN_PIN and DHAN_TOTP_SECRET" >&2
  exit 1
fi
set -a
# shellcheck disable=SC1091
source .env
set +a
scripts/questdb.sh status >/dev/null 2>&1 || scripts/questdb.sh start
cargo build --release -q -p tools --bin live-monitor
exec ./target/release/live-monitor "$@"
