#!/usr/bin/env bash
set -euo pipefail

: "${GH_TOKEN:?BOARD_SYNC_TOKEN is required}"
: "${PROJECT_NUMBER:?PROJECT_NUMBER repository variable is required}"
: "${PROJECT_OWNER:=DnaBoss}"
: "${FIELD_NAME:=Status}"

if [[ -z "${ITEM_URL:-}" && -n "${GITHUB_EVENT_PATH:-}" ]]; then
  ITEM_URL="$(jq -r '.issue.html_url // .pull_request.html_url // empty' "$GITHUB_EVENT_PATH")"
fi

if [[ -z "${ITEM_URL:-}" ]]; then
  echo "No event item; workflow dispatch is a permission/configuration check."
  gh project view "$PROJECT_NUMBER" --owner "$PROJECT_OWNER" >/dev/null
  exit 0
fi

item_id="$(gh project item-add "$PROJECT_NUMBER" --owner "$PROJECT_OWNER" \
  --url "$ITEM_URL" --format json --jq '.id')"
project_id="$(gh project view "$PROJECT_NUMBER" --owner "$PROJECT_OWNER" \
  --format json --jq '.id')"
field_json="$(gh project field-list "$PROJECT_NUMBER" --owner "$PROJECT_OWNER" \
  --format json --jq ".fields[] | select(.name == \"$FIELD_NAME\")")"
field_id="$(jq -r '.id' <<<"$field_json")"

case "${EVENT_NAME}:${EVENT_ACTION}" in
  issues:opened|issues:reopened) target="Backlog" ;;
  issues:closed) target="Done" ;;
  pull_request:opened|pull_request:reopened|pull_request:ready_for_review) target="Review" ;;
  pull_request:converted_to_draft) target="In progress" ;;
  pull_request:closed)
    if [[ "$(jq -r '.pull_request.merged // false' "$GITHUB_EVENT_PATH")" == "true" ]]; then
      target="Done"
    else
      target="In progress"
    fi
    ;;
  *) exit 0 ;;
esac

option_id="$(jq -r --arg target "$target" '.options[] | select(.name == $target) | .id' \
  <<<"$field_json")"
if [[ -z "$option_id" ]]; then
  echo "Status option not found: $target" >&2
  exit 1
fi

gh project item-edit --id "$item_id" --project-id "$project_id" \
  --field-id "$field_id" --single-select-option-id "$option_id" >/dev/null
echo "Moved $ITEM_URL to $target"
