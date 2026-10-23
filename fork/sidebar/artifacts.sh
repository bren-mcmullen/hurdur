#!/usr/bin/env bash
# hurdur fork: rows for the sidebar "artifacts" section.
#
# Lists the claude.ai artifacts most recently published or edited from any Claude Code session
# in the last 7 days, newest first. Prints one JSON item per line (the format is described in
# src/client/shell/fork_sections.rs). Each transcript's publishes are cached by size and mtime,
# so a run only re-reads transcripts that changed.
set -euo pipefail

limit="${ARTIFACTS_LIMIT:-10}"
projects="${CLAUDE_PROJECTS_DIR:-$HOME/.claude/projects}"
cache_dir="${XDG_STATE_HOME:-$HOME/.local/state}/hurdur-sidebar/artifacts"
mkdir -p "$cache_dir"
[ -d "$projects" ] || exit 0

file_stamp() {
  stat -f '%z-%m' "$1" 2>/dev/null || stat -c '%s-%Y' "$1"
}

caches=()
while IFS= read -r -d '' transcript; do
  key=$(printf '%s' "$transcript" | shasum | cut -c1-20)
  cache="$cache_dir/$key.tsv"
  stamp=$(file_stamp "$transcript")
  if [ ! -f "$cache" ] || [ "$(cat "$cache.stamp" 2>/dev/null)" != "$stamp" ]; then
    # Publish results carry the artifact's url, title and version.
    { grep -F '"toolUseResult":' "$transcript" || true; } \
      | { grep -F 'https://claude.ai/' || true; } \
      | jq -r 'select((.toolUseResult | type) == "object"
                 and ((.toolUseResult.url // "") | startswith("https://claude.ai/"))
                 and (.toolUseResult.version != null))
               | [.timestamp, .toolUseResult.url, (.toolUseResult.title // "untitled")]
               | @tsv' 2>/dev/null >"$cache.tmp" || true
    mv "$cache.tmp" "$cache"
    printf '%s' "$stamp" >"$cache.stamp"
  fi
  caches+=("$cache")
done < <(find "$projects" -type f -name '*.jsonl' -mtime -7 -print0)

[ "${#caches[@]}" -gt 0 ] || exit 0

cat "${caches[@]}" \
  | sort -r \
  | awk -F '\t' '!seen[$2]++' \
  | head -n "$limit" \
  | jq -R -c --argjson now "$(date -u +%s)" '
      split("\t") as [$timestamp, $url, $title]
      | ($now - ($timestamp | sub("\\.[0-9]+"; "") | fromdateiso8601)) as $age
      | (if $age < 3600 then "\([($age / 60 | floor), 1] | max)m"
         elif $age < 172800 then "\($age / 3600 | floor)h"
         else "\($age / 86400 | floor)d" end) as $ago
      | {
          url: $url,
          lines: [[
            {text: ((("   " + $ago) | .[-3:]) + " "), color: "dim"},
            {text: $title, color: "blue"}
          ]]
        }'
