#!/usr/bin/env bash
# Sets up Endurain for running-viz: admin (me), the dog accounts, monthly run goals,
# privacy defaults and API keys. Prints .env lines (keys, generated dog passwords) on stdout.
#
# Usage (from deploy/):
#   ENDURAIN_URL=http://localhost:8080 ADMIN_USERNAME=soeren ADMIN_PASSWORD='...' \
#   [SETUP_TOKEN='...' ADMIN_NAME='Soeren' ADMIN_EMAIL='me@example.com'] \
#   ../scripts/endurain-bootstrap.sh >> .env
#
# SETUP_TOKEN is only needed on a fresh instance (docker compose logs endurain | grep setup_token).
# Safe to re-run: existing users and goals are kept, new API keys are created on every run.
# Dog passwords: pass NANUQ_PASSWORD / ANORI_PASSWORD to keep them; otherwise (or if login with
# them fails) the admin sets a new random password, which is printed.
set -euo pipefail

: "${ENDURAIN_URL:?}" "${ADMIN_USERNAME:?}" "${ADMIN_PASSWORD:?}"
GOAL_KM_ADMIN="${GOAL_KM_ADMIN:-200}"
GOAL_KM_DOG="${GOAL_KM_DOG:-100}"
DOGS=(nanuq anori)
API="$ENDURAIN_URL/api/v1"

log() { echo "• $*" >&2; }
upper() { tr '[:lower:]' '[:upper:]' <<<"$1"; }
# curl that fails on HTTP errors (error body to stderr, never the request data)
call() {
  local out
  if ! out=$(curl -sS --fail-with-body "$@"); then
    echo "request failed: $out" >&2
    return 1
  fi
  printf '%s' "$out"
}
json() { call -H 'content-type: application/json' "$@"; }
form_login() { # username password -> raw login response
  call -X POST "$API/auth/login" -H 'X-Client-Type: web' \
    --data-urlencode "username=$1" --data-urlencode "password=$2"
}

login() { # username password -> access token; fails if a password change is required
  form_login "$1" "$2" | jq -er 'select(.next_step == "tokens") | .access_token'
}

etag() { # token url -> ETag of the resource
  curl -sS --fail -o /dev/null -D - -H "Authorization: Bearer $1" "$2" |
    awk 'tolower($1)=="etag:" {print $2}' | tr -d '\r' | grep .
}

set_dog_password() { # user_id username new_password: admin sets a temporary password, dog sets the final one
  local tmp res token
  tmp="Tmp-$(openssl rand -hex 12)!"
  json -X POST "$API/users/$1/password/change" -H "Authorization: Bearer $ADMIN" \
    -d "$(jq -n --arg p "$tmp" --arg a "$ADMIN_PASSWORD" '{password: $p, step_up: {current_password: $a}}')" >/dev/null
  res=$(form_login "$2" "$tmp")
  if [[ $(jq -r .next_step <<<"$res") == password_change ]]; then
    json -X POST "$API/auth/password-change" -H 'X-Client-Type: web' -d "$(jq -n \
      --arg c "$(jq -er .password_change_challenge <<<"$res")" --arg p "$3" \
      '{password_change_challenge: $c, password: $p}')" >/dev/null
  else
    token=$(jq -er .access_token <<<"$res")
    json -X POST "$API/profile/password/change" -H "Authorization: Bearer $token" \
      -d "$(jq -n --arg p "$3" --arg t "$tmp" '{password: $p, step_up: {current_password: $t}}')" >/dev/null
  fi
}

ensure_goal() { # token km
  local total
  total=$(call -H "Authorization: Bearer $1" \
    "$API/profile/goals?interval=monthly&activity_type=run&goal_type=distance" | jq -er '.total')
  if [[ "$total" == 0 ]]; then
    json -X POST "$API/profile/goals" -H "Authorization: Bearer $1" \
      -d "{\"interval\":\"monthly\",\"activity_type\":\"run\",\"goal_type\":\"distance\",\"goal_distance\":$(($2 * 1000))}" >/dev/null
    log "  goal: $2 km/month"
  else
    log "  goal exists, kept"
  fi
}

create_key() { # token password scopes-json -> key
  json -X POST "$API/profile/api-keys" -H "Authorization: Bearer $1" \
    -d "$(jq -n --arg p "$2" --argjson s "$3" '{name: "running-viz", scopes: $s, step_up: {current_password: $p}}')" |
    jq -er .key
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

# 2. Longest allowed API key lifetime (10 years)
settings=$(call -H "Authorization: Bearer $ADMIN" "$API/server-settings")
if [[ $(jq -er .api_key_max_lifetime_days <<<"$settings") != 3650 ]]; then
  json -X PUT "$API/server-settings" -H "Authorization: Bearer $ADMIN" \
    -H "If-Match: $(etag "$ADMIN" "$API/server-settings")" \
    -d "$(jq --arg p "$ADMIN_PASSWORD" 'del(.id, .version, .login_photo_set, .tileserver_api_key_set,
          .geocoding_api_key_set, .env_managed, .step_up_fields, .logs_viewer_enabled)
        + {api_key_max_lifetime_days: 3650, step_up: {current_password: $p}}' <<<"$settings")" >/dev/null
  log "api_key_max_lifetime_days = 3650"
fi

# 3. Admin goal + key
log "$ADMIN_USERNAME"
ensure_goal "$ADMIN" "$GOAL_KM_ADMIN"
key=$(create_key "$ADMIN" "$ADMIN_PASSWORD" '["activities:read","activities:upload","account"]')
echo "ENDURAIN_KEY_$(upper "$ADMIN_USERNAME")=$key"

# 4. Dogs
for dog in "${DOGS[@]}"; do
  log "$dog"
  pw_var="$(upper "$dog")_PASSWORD"
  pw="${!pw_var:-}"
  id=$(call -H "Authorization: Bearer $ADMIN" "$API/users?username=$dog" | jq -er 'if (.items | type) != "array" then error("bad user list")
    elif (.items | length) == 0 then ""
    else .items[0].id | if type == "string" and length > 0 then . else error("bad user id") end end')
  if [[ -z "$id" ]]; then
    id=$(json -X POST "$API/users" -H "Authorization: Bearer $ADMIN" -d "$(jq -n --arg u "$dog" \
      --arg p "Tmp-$(openssl rand -hex 12)!" --arg n "$(upper "${dog:0:1}")${dog:1}" \
      --arg e "$dog@${DOG_EMAIL_DOMAIN:-example.com}" \
      '{name: $n, username: $u, email: $e, password: $p, preferred_language: "en", gender: "unspecified",
        units: "metric", first_day_of_week: "monday", currency: "euro", timezone: "Europe/Berlin",
        access_type: "regular", active: true, email_verified: true}')" | jq -er .id)
    log "  created"
  fi
  if [[ -z "$pw" ]] || ! T=$(login "$dog" "$pw" 2>/dev/null); then
    pw="${pw:-$(openssl rand -base64 24)}"
    set_dog_password "$id" "$dog" "$pw"
    echo "${pw_var}=$pw"
    log "  password set"
    T=$(login "$dog" "$pw")
  fi
  ensure_goal "$T" "$GOAL_KM_DOG"
  json -X PATCH "$API/profile/privacy" -H "Authorization: Bearer $T" \
    -H "If-Match: $(etag "$T" "$API/profile/privacy")" \
    -d '{"default_activity_visibility":"followers","hide_activity_hr":true,"hide_activity_power":true,"hide_activity_cadence":true}' >/dev/null
  log "  privacy: followers, hide HR/power/cadence"
  key=$(create_key "$T" "$pw" '["activities:read","activities:upload","activities:write","account"]')
  echo "ENDURAIN_KEY_$(upper "$dog")=$key"
done
log "done"
