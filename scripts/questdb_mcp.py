#!/usr/bin/env python3
"""MCP server for the local QuestDB, so Claude can read and change it.

Speaks MCP over stdin/stdout and calls QuestDB's HTTP API. Standard library
only. Every statement QuestDB accepts is allowed, including ones that change
or delete data.
"""

import json
import os
import re
import sys
import urllib.error
import urllib.parse
import urllib.request

BASE_URL = os.environ.get("QUESTDB_URL", "http://127.0.0.1:9000")
DEFAULT_ROWS = 200
MAX_ROWS = 5000

TOOLS = [
    {
        "name": "list_tables",
        "description": "List the tables in QuestDB with their partitioning.",
        "inputSchema": {"type": "object", "properties": {}},
    },
    {
        "name": "describe_table",
        "description": "Show the columns and types of a QuestDB table.",
        "inputSchema": {
            "type": "object",
            "properties": {"table": {"type": "string"}},
            "required": ["table"],
        },
    },
    {
        "name": "query",
        "description": (
            "Run one QuestDB SQL statement of any kind: queries, INSERT, "
            "UPDATE, CREATE, ALTER, DROP and so on. Changes are permanent. "
            "Timestamps are UTC; IST is UTC+05:30."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "sql": {"type": "string"},
                "max_rows": {
                    "type": "integer",
                    "description": "Rows a query returns (default 200, at most 5000).",
                },
            },
            "required": ["sql"],
        },
    },
]


def run_sql(sql, rows):
    params = urllib.parse.urlencode({"query": sql, "limit": "0,{}".format(rows)})
    url = "{}/exec?{}".format(BASE_URL, params)
    try:
        with urllib.request.urlopen(url, timeout=120) as response:
            body = json.load(response)
    except urllib.error.HTTPError as e:
        try:
            raise RuntimeError(json.load(e).get("error", str(e)))
        except ValueError:
            raise RuntimeError(str(e))
    except urllib.error.URLError as e:
        raise RuntimeError(
            "cannot reach QuestDB at {} ({}); start it with scripts/questdb.sh start".format(
                BASE_URL, e.reason
            )
        )
    if "columns" not in body:
        return body  # the answer to a statement that changes something
    return {
        "columns": [c["name"] for c in body.get("columns", [])],
        "rows": body.get("dataset", []),
        "count": body.get("count"),
    }


def call_tool(name, args):
    if name == "list_tables":
        return run_sql(
            "SELECT table_name, designatedTimestamp, partitionBy FROM tables() ORDER BY 1",
            MAX_ROWS,
        )
    if name == "describe_table":
        table = str(args.get("table", ""))
        if not re.fullmatch(r"[A-Za-z0-9_]+", table):
            raise ValueError("bad table name")
        return run_sql("SHOW COLUMNS FROM {}".format(table), MAX_ROWS)
    if name == "query":
        sql = str(args.get("sql", ""))
        if not sql.strip():
            raise ValueError("sql is empty")
        try:
            rows = int(args.get("max_rows", DEFAULT_ROWS))
        except (TypeError, ValueError):
            raise ValueError("max_rows must be a whole number")
        return run_sql(sql, max(1, min(rows, MAX_ROWS)))
    raise ValueError("unknown tool {}".format(name))


def handle(request):
    method = request.get("method")
    params = request.get("params") or {}
    if method == "initialize":
        return {
            "protocolVersion": params.get("protocolVersion", "2024-11-05"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "questdb", "version": "0.1.0"},
        }
    if method == "ping":
        return {}
    if method == "tools/list":
        return {"tools": TOOLS}
    if method == "tools/call":
        try:
            text = json.dumps(call_tool(params.get("name"), params.get("arguments") or {}))
            return {"content": [{"type": "text", "text": text}]}
        except (ValueError, RuntimeError, OSError) as e:
            return {"content": [{"type": "text", "text": str(e)}], "isError": True}
    raise LookupError(method)


def main():
    for line in sys.stdin:
        if not line.strip():
            continue
        try:
            request = json.loads(line)
        except ValueError:
            continue
        if "id" not in request:
            continue  # a notification needs no answer
        reply = {"jsonrpc": "2.0", "id": request["id"]}
        try:
            reply["result"] = handle(request)
        except LookupError as e:
            reply["error"] = {"code": -32601, "message": "unknown method {}".format(e)}
        sys.stdout.write(json.dumps(reply) + "\n")
        sys.stdout.flush()


if __name__ == "__main__":
    main()
