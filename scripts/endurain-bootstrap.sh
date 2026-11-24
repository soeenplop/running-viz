#!/usr/bin/env bash
# Sets up Endurain for running-viz: admin (me), the dog accounts, monthly run goals,
# privacy defaults and API keys. Prints the keys as .env lines on stdout.
#
# Usage:
#   ENDURAIN_URL=http://localhost:8080 ADMIN_USERNAME=soeren ADMIN_PASSWORD=... \
#   [SETUP_TOKEN=... ADMIN_NAME=Soeren ADMIN_EMAIL=me@example.com] \
#   scripts/endurain-bootstrap.sh >> deploy/.env
#
# SETUP_TOKEN is only needed on a fresh instance (docker compose logs endurain | grep setup_token).
# Safe to re-run: existing users and goals are kept; new API keys are created each run.
set -euo pipefail

: "${ENDURAIN_URL:?}" "${ADMIN_USERNAME:?}" "${ADMIN_PASSWORD:?}"
GOAL_KM_ADMIN="${GOAL_KM_ADMIN:-200}"
GOAL_KM_DOG="${GOAL_KM_DOG:-100}"
DOGS=(nanuq anori)
API="$ENDURAIN_URL/api/v1"

log() { echo "• $*" >&2; }
json() { curl -sS --fail-with-body -H 'content-type: application/json' "$@"; }

login() { # username password -> access token (handles the first-login password change)
  local res
  res=$(curl -sS --fail-with-body -X POST "$API/auth/login" -H 'X-Client-Type: web' \
    --form-string "username=$1" --form-string "password=$2")
  if [[ $(jq -r .next_step <<<"$res") == password_change ]]; then
    res=$(json -X POST "$API/auth/password-change" -H 'X-Client-Type: web' \
      -d "$(jq -n --arg c "$(jq -r .password_change_challenge <<<"$res")" --arg p "$2" \
        '{password_change_challenge: $c, password: $p}')")
  fi
  jq -er .access_token <<<"$res"
}

etag() { curl -sS -o /dev/null -D - -H "Authorization: Bearer $1" "$2" | awk 'tolower($1)=="etag:" {print $2}' | tr -d '\r'; }

ensure_goal() { # token km
  local existing
  existing=$(curl -sS -H "Authorization: Bearer $1" "$API/profile/goals?interval=monthly&activity_type=run&goal_type=distance" | jq '.total')
  if [[ "$existing" == 0 ]]; then
    json -X POST "$API/profile/goals" -H "Authorization: Bearer $1" \
      -d "{\"interval\":\"monthly\",\"activity_type\":\"run\",\"goal_type\":\"distance\",\"goal_distance\":$(($2 * 1000))}" >/dev/null
    log "  goal: $2 km/month"
  else
    log "  goal exists, kept"
  fi
}

create_key() { # token password name scopes-json -> key
  json -X POST "$API/profile/api-keys" -H "Authorization: Bearer $1" \
    -d "$(jq -n --arg p "$2" --arg n "$3" --argjson s "$4" '{name: $n, scopes: $s, step_up: {current_password: $p}}')" | jq -er .key
}

# 1. Onboarding (fresh instance only)
if [[ -n "${SETUP_TOKEN:-}" ]]; then
  log "onboarding admin $ADMIN_USERNAME"
  json -X POST "$API/public/onboarding" -d "$(jq -n --arg n "${ADMIN_NAME:-$ADMIN_USERNAME}" \
    --arg u "$ADMIN_USERNAME" --arg e "${ADMIN_EMAIL:?ADMIN_EMAIL required with SETUP_TOKEN}" \
    --arg p "$ADMIN_PASSWORD" --arg t "$SETUP_TOKEN" \
    '{name: $n, username: $u, email: $e, password: $p, token: $t}')" >/dev/null
fi
ADMIN=$(login "$ADMIN_USERNAME" "$ADMIN_PASSWORD")
AUTH=(-H "Authorization: Bearer $ADMIN")

# 2. Longest allowed API key lifetime (10 years)
settings=$(curl -sS "${AUTH[@]}" "$API/server-settings")
if [[ $(jq .api_key_max_lifetime_days <<<"$settings") != 3650 ]]; then
  json -X PUT "$API/server-settings" "${AUTH[@]}" -H "If-Match: $(etag "$ADMIN" "$API/server-settings")" \
    -d "$(jq --arg p "$ADMIN_PASSWORD" 'del(.id, .version, .login_photo_set, .tileserver_api_key_set,
          .geocoding_api_key_set, .env_managed, .step_up_fields, .logs_viewer_enabled)
        + {api_key_max_lifetime_days: 3650, step_up: {current_password: $p}}' <<<"$settings")" >/dev/null
  log "api_key_max_lifetime_days = 3650"
fi

# 3. Admin goal + key
log "$ADMIN_USERNAME"
ensure_goal "$ADMIN" "$GOAL_KM_ADMIN"
echo "ENDURAIN_KEY_$(tr a-z A-Z <<<"$ADMIN_USERNAME")=$(create_key "$ADMIN" "$ADMIN_PASSWORD" running-viz \
  '["activities:read","activities:upload","account"]')"

# 4. Dogs
users=$(curl -sS "${AUTH[@]}" "$API/users?num_records=25")
for dog in "${DOGS[@]}"; do
  log "$dog"
  pw_var="$(tr a-z A-Z <<<"$dog")_PASSWORD"
  pw="${!pw_var:-$(openssl rand -base64 24)}"
  if ! jq -e --arg u "$dog" '.items | any(.username == $u)' <<<"$users" >/dev/null; then
    [[ -n "${!pw_var:-}" ]] || echo "${pw_var}=$pw"
    tmp="Tmp-$(openssl rand -hex 12)!"
    json -X POST "$API/users" "${AUTH[@]}" -d "$(jq -n --arg u "$dog" --arg p "$tmp" \
      --arg n "$(tr a-z A-Z <<<"${dog:0:1}")${dog:1}" --arg e "$dog@${DOG_EMAIL_DOMAIN:-example.com}" \
      '{name: $n, username: $u, email: $e, password: $p, preferred_language: "en", gender: "unspecified",
        units: "metric", first_day_of_week: "monday", currency: "euro", timezone: "Europe/Berlin",
        access_type: "regular", active: true, email_verified: true}')" >/dev/null
    # first login forces a password change: set the real password
    res=$(curl -sS -X POST "$API/auth/login" -H 'X-Client-Type: web' --form-string "username=$dog" --form-string "password=$tmp")
    json -X POST "$API/auth/password-change" -H 'X-Client-Type: web' -d "$(jq -n \
      --arg c "$(jq -r .password_change_challenge <<<"$res")" --arg p "$pw" \
      '{password_change_challenge: $c, password: $p}')" >/dev/null
    log "  created"
  elif [[ -z "${!pw_var:-}" ]]; then
    echo "error: $dog exists, set $pw_var to its password" >&2; exit 1
  fi
  T=$(login "$dog" "$pw")
  ensure_goal "$T" "$GOAL_KM_DOG"
  json -X PATCH "$API/profile/privacy" -H "Authorization: Bearer $T" -H "If-Match: $(etag "$T" "$API/profile/privacy")" \
    -d '{"default_activity_visibility":"followers","hide_activity_hr":true,"hide_activity_power":true,"hide_activity_cadence":true}' >/dev/null
  log "  privacy: followers, hide HR/power/cadence"
  echo "ENDURAIN_KEY_$(tr a-z A-Z <<<"$dog")=$(create_key "$T" "$pw" running-viz \
    '["activities:read","activities:upload","activities:write","account"]')"
done
log "done"
