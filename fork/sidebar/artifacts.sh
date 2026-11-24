#!/usr/bin/env bash
# hurdur fork: rows for the sidebar "artifacts" section.
#
# Lists the claude.ai artifacts most recently published or edited from any Claude Code session
# in the last 7 days, newest first. Database writes (ArtifactData) count as edits, and artifacts
# deleted from a Claude Code session are left out. Prints one
# JSON item per line (the format is described in src/client/shell/fork_sections.rs). Each
# transcript's publishes and writes are cached by size and mtime, so a run only re-reads
# transcripts that changed.
set -euo pipefail

limit="${ARTIFACTS_LIMIT:-10}"
projects="${CLAUDE_PROJECTS_DIR:-$HOME/.claude/projects}"
cache_dir="${XDG_STATE_HOME:-$HOME/.local/state}/hurdur-sidebar/artifacts"
mkdir -p "$cache_dir"
[ -d "$projects" ] || exit 0

titles="$cache_dir/titles.tsv"
touch "$titles"

file_stamp() {
  stat -f '%z-%m' "$1" 2>/dev/null || stat -c '%s-%Y' "$1"
}

# Newest publish title for artifact $1 in any transcript, however old. This reads every
# transcript, so callers save the answer in $titles and ask once per artifact.
lookup_title() {
  { find "$projects" -type f -name '*.jsonl' -print0 | xargs -0 grep -h -F "\"url\":\"$1\"" || true; } \
    | jq -r --arg url "$1" 'select(.toolUseResult.url? == $url and .toolUseResult.version? != null)
                            | [.timestamp, (.toolUseResult.title // "untitled")] | @tsv' 2>/dev/null \
    | sort | tail -n 1 | cut -f2
}

caches=()
while IFS= read -r -d '' transcript; do
  key=$(printf '%s' "$transcript" | shasum | cut -c1-20)
  cache="$cache_dir/$key.tsv"
  # v3: caches also hold database writes and deletes.
  stamp="v3-$(file_stamp "$transcript")"
  if [ ! -f "$cache" ] || [ "$(cat "$cache.stamp" 2>/dev/null)" != "$stamp" ]; then
    # Publish results carry the artifact's url, title and version. Database write results
    # carry only the url, so their title is left empty and filled in below. Delete results
    # get a fourth "deleted" field.
    { grep -F '"toolUseResult":' "$transcript" || true; } \
      | { grep -F 'https://claude.ai/' || true; } \
      | jq -r 'select((.toolUseResult | type) == "object")
               | .toolUseResult as $result
               | if (($result.url // "") | startswith("https://claude.ai/"))
                    and $result.version != null then
                   [.timestamp, $result.url, ($result.title // "untitled")]
                 elif (($result.written.url? // "") | startswith("https://claude.ai/"))
                    and $result.db_write.committed? == true then
                   [.timestamp, $result.written.url, ""]
                 elif (($result.artifact_delete.url? // "") | startswith("https://claude.ai/"))
                    and $result.artifact_delete.deleted? == true then
                   [.timestamp, $result.artifact_delete.url, "", "deleted"]
                 else empty end
               | @tsv' 2>/dev/null >"$cache.tmp" || true
    mv "$cache.tmp" "$cache"
    printf '%s' "$stamp" >"$cache.stamp"
  fi
  caches+=("$cache")
done < <(find "$projects" -type f -name '*.jsonl' -mtime -7 -print0)

[ "${#caches[@]}" -gt 0 ] || exit 0

rows=$(cat "${caches[@]}" | sort -r)

gone=$(awk -F '\t' '$4 == "deleted" { print $2 }' <<<"$rows")
if [ -n "$gone" ]; then
  { grep -v -F -f <(printf '%s\n' "$gone") "$titles" || true; } >"$titles.tmp"
  mv "$titles.tmp" "$titles"
fi

while IFS=$'\t' read -r timestamp url title; do
  if [ -z "$title" ]; then
    # Only database writes in the window: use the title from the artifact's last publish.
    title=$(awk -F '\t' -v url="$url" '$2 == url && $3 != "" && !found++ { print $3 }' <<<"$rows")
    [ -n "$title" ] || title=$(awk -F '\t' -v url="$url" '$1 == url && !found++ { print $2 }' "$titles")
    if [ -z "$title" ]; then
      title=$(lookup_title "$url" || true)
      title="${title:-untitled}"
      printf '%s\t%s\n' "$url" "$title" >>"$titles"
    fi
  fi
  printf '%s\t%s\t%s\n' "$timestamp" "$url" "$title"
done < <(awk -F '\t' -v limit="$limit" '
           # A delete is the newest row for its artifact, so it is read before the rows it hides.
           $4 == "deleted" { gone[$2] }
           NF >= 2 && !($2 in gone) && !seen[$2]++ && shown++ < limit' <<<"$rows") \
  | jq -R -c --argjson now "$(date -u +%s)" '
      split("\t") as [$timestamp, $url, $title]
      | ($now - ($timestamp | sub("\\.[0-9]+"; "") | fromdateiso8601)) as $age
      | (if $age < 3600 then "\([($age / 60 | floor), 1] | max)m"
         elif $age < 172800 then "\($age / 3600 | floor)h"
         else "\($age / 86400 | floor)d" end) as $ago
      | {
          url: $url,
          lines: [[
            {text: ((("    " + $ago) | .[-4:]) + " "), color: "dim"},
            {text: $title, color: "blue"}
          ]]
        }'
