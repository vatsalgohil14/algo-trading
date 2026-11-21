#!/usr/bin/env bash
# Evening job for the momentum rotation, after NSE closes (run after ~18:00 IST):
# brings bars_daily up to date from Dhan, then advances the paper account and
# prints the report (also in data/paper/<name>/report.txt). Places no orders.
#
#   scripts/rotation_daily.sh                  # the 'default' paper account
#   scripts/rotation_daily.sh --name replay    # another account
#
# Start an account first:
#   cargo run --release -p tools --bin rotation-paper -- init --capital 1000000
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
cargo build --release -q -p tools --bin dhan-daily --bin rotation-paper
# The last 10 days: re-downloading a few days is harmless (one row per day
# and stock) and picks up any day a previous run missed.
from=$(date -v-10d +%F 2>/dev/null || date -d '10 days ago' +%F)
mkdir -p data/paper
./target/release/dhan-daily --from "$from" > data/paper/dhan_daily_last.log 2>&1 \
  || { echo "dhan-daily failed; see data/paper/dhan_daily_last.log" >&2; exit 1; }
tail -n 3 data/paper/dhan_daily_last.log
exec ./target/release/rotation-paper run "$@"
