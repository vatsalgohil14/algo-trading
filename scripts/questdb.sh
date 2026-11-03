#!/usr/bin/env bash
# Manage the local QuestDB instance.
#
#   scripts/questdb.sh start | stop | restart | status
#   scripts/questdb.sh init            # create/upgrade tables from deploy/questdb/schema.sql
#   scripts/questdb.sh sql "SELECT 1"  # run one statement, print CSV-ish output
#   scripts/questdb.sh logs            # follow the server log
#
# Install locations can be overridden with env vars:
#   QDB_HOME   QuestDB install   (default ~/.local/opt/questdb)
#   JAVA_HOME  Java 25 runtime   (default ~/.local/opt/temurin-25-jre/Contents/Home)
#   QDB_ROOT   data directory    (default ~/.questdb)

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
QDB_HOME="${QDB_HOME:-$HOME/.local/opt/questdb}"
export JAVA_HOME="${JAVA_HOME:-$HOME/.local/opt/temurin-25-jre/Contents/Home}"
QDB_ROOT="${QDB_ROOT:-$HOME/.questdb}"
HTTP="http://127.0.0.1:9000"
HEALTH="http://127.0.0.1:9003"

die() { echo "error: $*" >&2; exit 1; }

check_install() {
  [[ -x "$QDB_HOME/questdb.sh" ]] || die "QuestDB not found at $QDB_HOME"
  [[ -x "$JAVA_HOME/bin/java" ]] || die "Java not found at $JAVA_HOME"
}

is_up() { curl -fsS -o /dev/null --max-time 2 "$HEALTH" 2>/dev/null; }

# Runs one SQL statement over the HTTP API. Exits non-zero on SQL errors.
sql() {
  local out
  out="$(curl -sS -G --max-time 120 "$HTTP/exec" --data-urlencode "query=$1")" \
    || die "QuestDB is not reachable at $HTTP"
  if [[ "$out" == *'"error"'* ]]; then
    echo "$out" >&2
    return 1
  fi
  printf '%s\n' "$out"
}

cmd_start() {
  check_install
  if is_up; then echo "QuestDB already running"; return; fi
  if [[ ! -f "$QDB_ROOT/conf/server.conf" ]]; then
    mkdir -p "$QDB_ROOT/conf"
    cp "$REPO_ROOT/deploy/questdb/server.conf" "$QDB_ROOT/conf/server.conf"
    echo "Installed config: $QDB_ROOT/conf/server.conf"
  fi
  "$QDB_HOME/questdb.sh" start -d "$QDB_ROOT" >/dev/null
  for _ in $(seq 1 60); do
    if is_up; then
      echo "QuestDB is up: console $HTTP, Postgres wire 127.0.0.1:8812 (admin/quest), ILP 127.0.0.1:9009"
      return
    fi
    sleep 0.5
  done
  die "QuestDB did not become healthy within 30s; see: $0 logs"
}

cmd_stop() {
  check_install
  "$QDB_HOME/questdb.sh" stop -d "$QDB_ROOT" >/dev/null || true
  echo "QuestDB stopped"
}

cmd_status() {
  if is_up; then
    echo "QuestDB running ($HTTP)"
  else
    echo "QuestDB not running"
    return 1
  fi
}

# Applies schema.sql one statement at a time. Statements are separated by
# lines ending in ';'; every statement must be idempotent (IF NOT EXISTS).
cmd_init() {
  is_up || die "QuestDB is not running; start it first"
  local schema="$REPO_ROOT/deploy/questdb/schema.sql" stmt="" n=0
  [[ -f "$schema" ]] || die "missing $schema"
  while IFS= read -r line || [[ -n "$line" ]]; do
    line="${line%%--*}"                       # strip comments
    [[ -z "${line//[[:space:]]/}" ]] && continue
    stmt+="$line"$'\n'
    if [[ "$line" =~ \;[[:space:]]*$ ]]; then
      sql "${stmt%;*}" >/dev/null || die "failed statement:"$'\n'"$stmt"
      n=$((n + 1))
      stmt=""
    fi
  done < "$schema"
  [[ -z "${stmt//[[:space:]]/}" ]] || die "schema.sql ends without ';'"
  echo "Applied $n statements from deploy/questdb/schema.sql"
}

cmd_logs() {
  local latest
  latest="$(ls -t "$QDB_ROOT"/log/*.txt 2>/dev/null | head -1)" || true
  [[ -n "$latest" ]] || die "no log files in $QDB_ROOT/log"
  tail -f "$latest"
}

case "${1:-}" in
  start) cmd_start ;;
  stop) cmd_stop ;;
  restart) cmd_stop; cmd_start ;;
  status) cmd_status ;;
  init) cmd_init ;;
  sql) shift; [[ $# -ge 1 ]] || die "usage: $0 sql \"<statement>\""; sql "$1" ;;
  logs) cmd_logs ;;
  *) sed -n '2,13p' "$0"; exit 1 ;;
esac
