#!/usr/bin/env bash
set -euo pipefail

project_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
plugin="${DSH_WEBCLX_PLUGIN:-${CARGO_TARGET_DIR:-$project_root/target}/debug/examples/webclx_terminal_message_plugin}"
base_url="${WEBCLX_URL:-http://127.0.0.1:11111}"
token_file="${WEBCLX_LOCAL_TOKEN_FILE:-/home/bin/webclx/.webclx-local-api-token}"
expected_path="${DSH_WEBCLX_EXPECTED_PATH:-deepseekHarnessRs}"
read_timeout="${DSH_WEBCLX_ACCEPT_TIMEOUT_SECONDS:-15}"

if [ ! -x "$plugin" ]; then
  printf 'webClx plugin is not executable: %s\n' "$plugin" >&2
  exit 1
fi

args=(--base-url "$base_url")
if [ -r "$token_file" ]; then
  args+=(--local-token-file "$token_file")
fi

coproc WEBCLX_PLUGIN {
  DSH_PLUGIN_PROTOCOL=1 DSH_PLUGIN_ID=webclx-terminal-message \
    "$plugin" "${args[@]}"
}
plugin_pid="$WEBCLX_PLUGIN_PID"

cleanup() {
  kill "$plugin_pid" 2>/dev/null || true
  wait "$plugin_pid" 2>/dev/null || true
}
trap cleanup EXIT HUP INT TERM

printf '%s\n' \
  '{"version":1,"type":"hello","plugin_id":"webclx-terminal-message"}' \
  >&"${WEBCLX_PLUGIN[1]}"
IFS= read -r -t "$read_timeout" hello <&"${WEBCLX_PLUGIN[0]}"
jq -e '
  .version == 1 and .type == "hello" and
  .plugin_id == "webclx-terminal-message" and
  ([.tools[].name] | sort) == ["webclx_list_terminals", "webclx_send_terminal_message"]
' <<<"$hello" >/dev/null

call="$(jq -nc --arg path "$expected_path" '{
  version: 1,
  type: "call",
  id: 1,
  tool: "webclx_list_terminals",
  arguments: {path: $path, alive_only: true}
}')"
printf '%s\n' "$call" >&"${WEBCLX_PLUGIN[1]}"
IFS= read -r -t "$read_timeout" result <&"${WEBCLX_PLUGIN[0]}"
jq -e --arg path "$expected_path" '
  .version == 1 and .type == "result" and .id == 1 and .ok == true and
  (.value | type == "array") and (.value | length >= 1) and
  all(.value[];
    .alive == true and
    ((.path == $path) or (.display_path | endswith("/" + $path)))
  )
' <<<"$result" >/dev/null

printf 'webClx terminal-message adapter accepted NDJSON and listed %s matching terminal(s)\n' \
  "$(jq '.value | length' <<<"$result")"
