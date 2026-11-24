# Implementation plan

Goal: a monthly running challenge (me 200 km, Nanuq 100 km, Anori 100 km, repeating every month), shown on a 4.2" e-paper display and as a fullscreen web page. Guiding principle: **keep it simple**.

## 1. Data flow

```
Apple Watch run ─▶ HealthFit ─▶ Dropbox (/Apps/HealthFit/*.fit)
                                     │  poll every 5 min
                                     ▼
                         running-viz server (Rust)
   Shortcut "Who came along?" ──▶ POST /api/companions
                                     │
       ┌──────────────── uploads ────┼──────────────────────┐
       ▼                             ▼                      ▼
 Endurain: my account        Endurain: Nanuq          Endurain: Anori
 (original FIT)              (GPX copy)               (GPX copy)
       └──────────── monthly stats + goals (read) ──────────┘
                                     │
                     renders 400×300 1-bit image
                    ┌────────────────┴───────────────┐
           GET /display.bin                    GET /display.png
           ESP8266 + e-paper                   web page (fullscreen)
```

Endurain is the source of truth for activities and goals. The server's own Postgres only stores import state and the run ↔ copy mapping.

## 2. Verified Endurain facts (v0.21.0, read from a local `/openapi.json`)

- Auth: `X-API-Key: endurain_…`. Keys expire (default max 365 days; admin setting `api_key_max_lifetime_days`).
- Upload: `POST /api/v1/activities/upload`, multipart field `file` (`.fit`/`.gpx`/`.tcx`/`.gz`), scope `activities:upload`. Returns **202 + job**; poll `GET /api/v1/jobs/{id}` until `status` is final, then read `activity_ids`. Supports `Idempotency-Key`. Duplicates (same file content + start time) are skipped.
- List: `GET /api/v1/activities?start_date=&end_date=&type=&page_number=&num_records=` (scope `activities:read`). Items include `distance` (m), `total_timer_time` (s), `start_time`, `timezone`, `activity_type`.
- Edit/delete: `PATCH|DELETE /api/v1/activities/{id}` with `If-Match` (ETag/version). Editable: `name`, `visibility`, `private_notes`, `hide_*` flags, …
- Goals: `GET /api/v1/profile/goals` and `/profile/goals/results`, scope `account`. Fields: `interval=monthly`, `activity_type=run`, `goal_type=distance`, `goal_distance` (m).
- Users: admin UI (or `POST /api/v1/users`). Privacy defaults: `PATCH /api/v1/profile/privacy` (`default_activity_visibility`, `hide_activity_hr`, …).
- Rate limits: 120 req/min default, 20 uploads/min. Plenty for our use.

## 3. Components

### 3.1 `deploy/` – Docker Compose

Services: `endurain` (pinned image digest), `db` (one postgres 18 with two databases and roles: `endurain` and `running_viz`, created by an init script), `server` (added in PR 3). `.env.example` documents every variable. An opt-in `compose.dev.yaml` (`docker compose -f compose.yaml -f compose.dev.yaml up`) exposes ports and sets Endurain `ENVIRONMENT=development` (OpenAPI available); production never loads it.

One-time Endurain setup (documented in `docs/setup.md`, done by hand in the UI, no automation):
1. Create admin (me) via setup token.
2. Create users `nanuq` and `anori`.
3. In each account: monthly run distance goal (200 / 100 / 100 km) and an API key. My key: `activities:read activities:upload account`. Dog keys additionally `activities:write`. Raise the admin setting `api_key_max_lifetime_days` so keys don't expire after a year.
4. Dog accounts: default visibility `followers`, hide HR/cadence/power. I follow both dogs.

### 3.2 `server/` – Rust

Stack: `axum`, `tokio`, `sqlx` (Postgres + migrations), `reqwest` (rustls), `serde`, `fitparser`, `embedded-graphics` + `u8g2-fonts`, `png`, `chrono` + `chrono-tz`, `tracing`. Config from env.

**Background loop** (one tokio task, every 5 min). Every step persists its state *before* the next external call, so a restart resumes where it stopped:
1. **Dropbox fetch**: `list_folder` / `list_folder/continue` (Dropbox refresh token + app key/secret in env). Each page's new `.fit` entries (path, `content_hash`, **file bytes**) and the next cursor are written in **one transaction**. Keeping the FIT bytes in Postgres (small files) makes late claims and re-copies possible.
2. **Upload worker**: works through `uploads` rows by status, independent of Dropbox:
   `pending → (POST upload, store job_id) → job_started → (poll job, store activity_id) → uploaded → (PATCH, copies only) → done`.
   - Idempotency key is deterministic: `fit:<content_hash>` for originals, `run:<run_id>:<dog>:<generation>` for copies. A replay after a crash returns the same job.
   - Job succeeded with **0 ids** (Endurain skipped a duplicate) → look the activity up by start time via the list endpoint. **>1 ids** → take the first, log the rest.
   - Original done → the run gets sport, start/end time, distance, has GPS from the activity.
3. **Companion matching**: see 3.3.
4. **Dog copies**: for each (run, dog) that should exist: FIT → GPX (only timestamp, lat/lon, elevation) → upload row with the dog's key → after upload, `PATCH` `name` and `activity_type` (= original's type, so the copy counts toward the run goal regardless of GPX sport tags). Dog removed → delete the copy. Re-added → `generation + 1` = fresh upload.
5. **Stats refresh**: for each of the 3 accounts: monthly run goal + current month's activities (Europe/Berlin, paginated) of types `run`, `trail_run`, `track_run` → total km, avg pace (Σ timer time / Σ distance), km per day → in-memory state → re-render; ETag = hash of the bitmap. On upstream failure the last good stats stay visible.
6. **Reconcile** (once a day): only originals *with copies*, current and previous month. A `404` means deleted → delete the copies. Auth errors and timeouts are just retried, never treated as deletion. Conditional writes always fetch a fresh version first. `Retry-After` is honored.

**HTTP API** (`running-api.soeenplop.dev`):
| Method | Path | Auth | Purpose |
|---|---|---|---|
| GET | `/healthz` | – | liveness + last sync time |
| GET | `/display.bin` | – | 15 000 bytes, 1 bpp, MSB first, 1 = white; `ETag`, `304` on `If-None-Match` |
| GET | `/display.png` | – | same image as PNG |
| POST | `/api/companions` | `Bearer SHORTCUT_TOKEN` | `{"dogs": ["nanuq","anori"], "ended_at": "…"?, "run": "latest"?}` (empty list = nobody) |

The public routes are an explicit owner decision: the image (incl. km per day) may be public. Tokens are redacted from logs. `/api/companions` limits the body size and accepts at most one claim per minute. Detailed sync/auth errors only go to the server logs.

**DB tables** (`running_viz` database):
- `dropbox_state(cursor)`
- `fit_files(id, dropbox_path, content_hash UNIQUE, bytes, received_at)`
- `runs(id, fit_file_id, endurain_activity_id, sport, start_time, end_time, distance_m, has_gps)`
- `uploads(id, kind[original|copy], fit_file_id, run_id NULL, dog NULL, generation, idempotency_key UNIQUE, job_id, endurain_activity_id, status, attempts, last_error)`
- `companion_claims(id, received_at, ended_at NULL, target[auto|latest], dogs, run_id NULL, status[pending|matched|expired])`

### 3.3 Matching companions to runs

The Shortcut is triggered by the Apple Watch automation **when the workout ends**, so the claim time ≈ the run's end time. The FIT file arrives minutes later.
- Reference time `t` = `ended_at` if the Shortcut sends it (latest workout end date from Health, if the Shortcut can read it), else `received_at`.
- A claim matches only a run with `end_time` in **[t − 15 min, t + 2 min]**. No match yet → stays `pending`, retried every loop, `expired` after 24 h. More than one candidate → stays pending and is logged (no guessing).
- **Correction**: re-running the Shortcut manually sends `"run": "latest"` → applies to the most recent run. The newest claim per run wins: missing dogs get their copy deleted, new dogs get one.
- Only running types with GPS get copies.
- Tests: delayed FIT, claim after FIT, reversed order of two runs, duplicate claim, empty dog list, ambiguity.

### 3.4 Display layout (400×300, 1 bit)

Based on the approved mockup, plus average pace:

```
┌────────────────────────────────────────────────────┐
│ DEZEMBER-CHALLENGE                  NOCH 17 TAGE   │
├──────────────────────────────┬─────────────────────┤
│ SOEREN                       │ NANUQ               │
│ 69,6 / 200 KM                │ 46,7 / 100 KM       │
│ ████████░░░░░░░░░░░░░░░░░░   │ ████░░░░░░          │
│ Ø 5:42 /KM                   │ Ø 5:38 /KM          │
│                              │ ANORI …             │
├──────────────────────────────┴─────────────────────┤
│ MO  DI  MI  DO  FR  SA  SO   (my km per day,       │
│      1   2   3 …              today inverted)      │
└────────────────────────────────────────────────────┘
```

- German labels and month names on the display (it's for us); code stays English.
- Renderer is a pure function `render(&Stats, today) -> Bitmap`; snapshot tests with fixture stats.
- `/display.png` is that bitmap scaled 1:1; the web page scales it with `image-rendering: pixelated`.

### 3.5 `web/` – fullscreen page

One `index.html` (no build step), embedded into the server binary and served at `/` (domain `running.soeenplop.dev` points at the same container). Shows `/display.png` centered on a dark background, reloads it every 5 min, fullscreen on click.

### 3.6 `firmware/` – ESP8266 (C++, PlatformIO, battery)

- Board: Waveshare e-Paper ESP8266 Driver Board; panel 4.2" 400×300 (GxEPD2 driver; exact class to be confirmed on hardware: `GxEPD2_420` vs `GxEPD2_420_GDEY042T81`).
- Wake → WiFi → `GET /display.bin` with the stored ETag (RTC memory) → `304`: nothing; `200`: stream the bytes straight into the panel RAM (no 15 KB heap buffer), refresh **only if exactly 15 000 bytes arrived**, then save the ETag → panel hibernate → deep sleep **fixed 30 min**, also after any failure.
- Deadlines: WiFi 15 s, HTTP/TLS 15 s, whole wake 45 s.
- HTTPS with `BearSSL::WiFiClientSecure::setInsecure()`: no certificate check. Deliberate: the image is public and the worst case of a MITM is a wrong picture; NTP + trust anchors on an ESP8266 are not worth it.
- WiFi credentials + API URL in a gitignored `secrets.h` (`secrets.example.h` committed).
- **Hardware smoke test right after the renderer PR** (when the owner attaches the board): GPIO16 → RST wake, memory with TLS, the 304 path, readability of a busy month, sleep current. Fallback if deep sleep doesn't work: USB power.

## 4. Quality

- Rust: `cargo fmt`, `clippy -D warnings`, unit tests (GPX conversion, matching, stats incl. Europe/Berlin month boundary and an empty month, renderer snapshots, upload state machine incl. restart after the 202 response and before the PATCH), one ignored-by-default integration test against a local Endurain.
- **Acceptance test with a real HealthFit FIT** (with pauses): import the original, the GPX copy goes to a dog account; check activity type, goal contribution, distance/timer time against the original, and that the GPX contains no HR/cadence/power.
- Firmware: `pio run` must compile.
- GitHub Actions: Rust checks + PlatformIO build.

## 5. Delivery: PRs in order

| # | Branch | Title |
|---|---|---|
| 1 | `docs/plan` | `docs: add implementation plan` |
| 2 | `feat/compose` | `feat(deploy): docker compose with endurain` + `docs/setup.md` (Endurain + db only) |
| 3 | `feat/server-skeleton` | `feat(server): axum skeleton, config, migrations, dockerfile, ci` (adds `server` to compose) |
| 4 | `feat/renderer` | `feat(server): render display image and serve png/bin` (fixture stats) → hardware smoke test when the board is attached |
| 5 | `feat/web` | `feat(web): fullscreen display page` |
| 6 | `feat/endurain-stats` | `feat(server): monthly stats and goals from endurain` |
| 7 | `feat/dropbox-import` | `feat(server): import healthfit fit files from dropbox` |
| 8 | `feat/dog-copies` | `feat(server): companions endpoint and dog copies` + Shortcut guide |
| 9 | `feat/firmware` | `feat(firmware): esp8266 e-paper client` |
| 10 | `feat/dokploy` | `chore(deploy): dokploy deployment` (needs user, last) |

Each PR: Conventional Commits, review by GPT-6.2 Astra in PR comments, squash-merge after `✅ Approved by GPT-6.2 Astra`.

## 6. Risks and open points

- **Dropbox auth** needs a one-time OAuth flow by the user (app key/secret → refresh token). Documented in `docs/setup.md`.
- **API key expiry** (≤ 365 days): `/healthz` reports auth failures; setup doc recommends raising the max lifetime.
- **Endurain feature freeze**: we pin the image digest and rely only on endpoints listed above.
- **HealthFit folder path and sport names** in FIT files: confirm with a real export.
- **Panel variant and deep-sleep wiring**: confirm on hardware.
