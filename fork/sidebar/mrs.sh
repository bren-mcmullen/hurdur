#!/usr/bin/env bash
# hurdur fork: rows for the sidebar "mrs" section.
#
# Lists my GitLab MRs active in the last 7 days with a plain-language status. Merged MRs stay
# until their post-merge pipeline passes, or for a day after merge when it fails or waits on a
# manual job. Prints one JSON item per line (the format is described in
# src/client/shell/fork_sections.rs). Exits non-zero on any GitLab error so herdr keeps the
# previous rows.
set -euo pipefail

limit="${MRS_LIMIT:-8}"
max_checked="${MRS_MAX_CHECKED:-25}"
state_dir="${XDG_STATE_HOME:-$HOME/.local/state}/hurdur-sidebar"
# project_id:iid of merged MRs that are finished (post-merge pipeline passed, or none ran).
done_file="$state_dir/mrs-done.txt"
mkdir -p "$state_dir"
touch "$done_file"

since=$(date -u -v-7d +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || date -u -d '7 days ago' +%Y-%m-%dT%H:%M:%SZ)
now=$(date -u +%s)

api() {
  glab api "$1" 2>/dev/null
}

mark_done() {
  printf '%s\n' "$1" >>"$done_file"
}

running_states='created|waiting_for_resource|preparing|pending|running|scheduled'

# The status helpers run in $(...) subshells, which don't inherit errexit on bash 3.2, so every
# GitLab call in them returns explicitly on failure.

# Prints "<color>\t<icon>\t<text>" for an open MR.
open_status() {
  local pid=$1 iid=$2 detail pipeline merge_status draft conflicts
  detail=$(api "projects/$pid/merge_requests/$iid") || return 1
  pipeline=$(jq -r '.head_pipeline.status // ""' <<<"$detail")
  merge_status=$(jq -r '.detailed_merge_status // ""' <<<"$detail")
  draft=$(jq -r '.draft // false' <<<"$detail")
  conflicts=$(jq -r '.has_conflicts // false' <<<"$detail")
  if [ "$draft" = true ]; then
    printf 'dim\t◌\tdraft\n'
  elif [ "$pipeline" = failed ]; then
    printf 'red\t✗\tpipeline failed\n'
  elif [ "$pipeline" = manual ]; then
    printf 'yellow\t▶\tneeds manual action\n'
  elif [[ "$pipeline" =~ ^($running_states)$ ]]; then
    printf 'blue\t◐\tpipeline running\n'
  elif [ "$conflicts" = true ] || [[ "$merge_status" =~ ^(conflict|need_rebase)$ ]]; then
    printf 'red\t!\tneeds rebase\n'
  elif [ "$merge_status" = discussions_not_resolved ]; then
    printf 'yellow\t○\tunresolved threads\n'
  elif [ "$merge_status" = requested_changes ]; then
    printf 'yellow\t○\tchanges requested\n'
  elif [ "$merge_status" = blocked_status ]; then
    printf 'yellow\t○\tblocked\n'
  elif [ "$merge_status" = not_approved ]; then
    printf 'yellow\t○\tneeds approval\n'
  elif [ "$merge_status" = mergeable ]; then
    printf 'green\t✓\tready to merge\n'
  else
    printf 'dim\t·\t%s\n' "${merge_status//_/ }"
  fi
}

# Prints "<color>\t<icon>\t<text>" for a merged MR, or nothing once it is finished.
merged_status() {
  local pid=$1 iid=$2 sha=$3 target=$4 merged_at=$5 pipeline status pipeline_id newer
  pipeline=$(api "projects/$pid/pipelines?sha=$sha&ref=$(jq -rn --arg ref "$target" '$ref | @uri')&per_page=1") || return 1
  status=$(jq -r '.[0].status // ""' <<<"$pipeline")
  pipeline_id=$(jq -r '.[0].id // 0' <<<"$pipeline")
  case "$status" in
    success)
      mark_done "$pid:$iid"
      ;;
    "")
      if [ $((now - merged_at)) -lt 600 ]; then
        printf 'dim\t◌\tmerged · waiting for pipeline\n'
      else
        mark_done "$pid:$iid"
      fi
      ;;
    manual)
      # Some post-merge manual jobs are never run; stop showing them a day after merge.
      if [ $((now - merged_at)) -ge 86400 ]; then
        mark_done "$pid:$iid"
      else
        printf 'yellow\t▶\tmerged · needs manual action\n'
      fi
      ;;
    failed | canceled | skipped)
      # A later passing pipeline on the target branch includes this change. Either way, stop
      # showing it a day after merge.
      newer=$(api "projects/$pid/pipelines?ref=$(jq -rn --arg ref "$target" '$ref | @uri')&status=success&per_page=1" \
        | jq -r '.[0].id // 0') || return 1
      if [ "$newer" -gt "$pipeline_id" ] || [ $((now - merged_at)) -ge 86400 ]; then
        mark_done "$pid:$iid"
      elif [ "$status" = failed ]; then
        printf 'red\t✗\tmerged · pipeline failed\n'
      else
        printf 'dim\t–\tmerged · pipeline %s\n' "$status"
      fi
      ;;
    *)
      if [[ "$status" =~ ^($running_states)$ ]]; then
        printf 'blue\t◐\tmerged · pipeline running\n'
      else
        printf 'dim\t·\tmerged · pipeline %s\n' "${status//_/ }"
      fi
      ;;
  esac
}

open=$(api "merge_requests?scope=created_by_me&state=opened&updated_after=$since&order_by=updated_at&per_page=50")
merged=$(api "merge_requests?scope=created_by_me&state=merged&updated_after=$since&order_by=updated_at&per_page=50")

candidates=$(jq -nc --argjson open "$open" --argjson merged "$merged" '
  [$open[], $merged[]]
  | sort_by(.updated_at) | reverse | .[]
  | {
      key: "\(.project_id):\(.iid)",
      pid: .project_id,
      iid: .iid,
      state,
      title,
      url: .web_url,
      project: (.references.full | split("!")[0] | split("/") | last),
      sha: (.merge_commit_sha // .squash_commit_sha // .sha // ""),
      target: .target_branch,
      merged_at: ((.merged_at // "1970-01-01T00:00:00Z") | sub("\\.[0-9]+"; "") | fromdateiso8601)
    }')

shown=0
checked=0
while IFS= read -r mr; do
  [ -n "$mr" ] || continue
  [ "$shown" -lt "$limit" ] && [ "$checked" -lt "$max_checked" ] || break
  key=$(jq -r .key <<<"$mr")
  grep -qxF "$key" "$done_file" && continue
  checked=$((checked + 1))
  IFS=$'\t' read -r pid iid state sha target merged_at < <(jq -r '[.pid, .iid, .state, .sha, .target, .merged_at] | @tsv' <<<"$mr")
  if [ "$state" = merged ]; then
    status=$(merged_status "$pid" "$iid" "$sha" "$target" "$merged_at")
  else
    status=$(open_status "$pid" "$iid")
  fi
  [ -n "$status" ] || continue
  IFS=$'\t' read -r color icon text <<<"$status"
  jq -nc --argjson mr "$mr" --arg color "$color" --arg status "  $icon $text" '{
    url: $mr.url,
    lines: [
      [{text: "\($mr.project) !\($mr.iid) ", color: "dim"}, {text: $mr.title, color: "text"}],
      [{text: $status, color: $color}]
    ]
  }'
  shown=$((shown + 1))
done <<<"$candidates"
