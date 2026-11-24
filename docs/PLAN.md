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

Services: `endurain` (pinned image digest), `endurain-db` (postgres 18), `server`, `server-db` (postgres 18, separate instance; no shared DB to keep ownership clear). `.env.example` documents every variable. A `compose.override.yaml` for local dev exposes ports and sets Endurain `ENVIRONMENT=development` (OpenAPI available).

One-time Endurain setup (documented in `docs/setup.md`, done by hand in the UI, no automation):
1. Create admin (me) via setup token.
2. Create users `nanuq` and `anori`.
3. In each account: monthly run distance goal (200 / 100 / 100 km), API key with scopes `activities:read activities:upload activities:write account`.
4. Dog accounts: default visibility `followers`, hide HR/cadence/power. I follow both dogs.

### 3.2 `server/` – Rust

Stack: `axum`, `tokio`, `sqlx` (Postgres + migrations), `reqwest` (rustls), `serde`, `fitparser`, `embedded-graphics` + `u8g2-fonts`, `png`, `chrono` + `chrono-tz`, `tracing`. Config from env.

**Background loop** (one tokio task, every 5 min, plus on demand):
1. **Dropbox import**: `list_folder` / `list_folder/continue` with stored cursor (Dropbox refresh token + app key/secret in env). For each new `.fit`: download → upload to my Endurain account (`Idempotency-Key` = Dropbox `content_hash`) → poll job → store run (activity id, start/end time, sport, distance, has GPS).
2. **Companion matching**: attach unmatched companion claims to runs (see 3.3).
3. **Dog copies**: for each run with dogs and no copy yet: FIT → GPX (only timestamp, lat/lon, elevation; nothing else) → upload with the dog's key → poll job → `PATCH` name (same as original) → store copy id. Removed dogs: delete their copy.
4. **Reconcile**: if an original no longer exists in Endurain (404), delete its copies.
5. **Stats refresh**: for each of the 3 accounts: monthly run goal + current month's run activities (Europe/Berlin) → totals, avg pace (Σ timer time / Σ distance), km per day → in-memory state → re-render image, new ETag.

**HTTP API** (`running-api.soeenplop.dev`):
| Method | Path | Auth | Purpose |
|---|---|---|---|
| GET | `/healthz` | – | liveness + last sync time |
| GET | `/display.bin` | – | 15 000 bytes, 1 bpp, MSB first, 1 = white; `ETag`, `304` on `If-None-Match` |
| GET | `/display.png` | – | same image as PNG |
| GET | `/api/summary` | – | JSON of the numbers on the display (debugging) |
| POST | `/api/companions` | `Bearer SHORTCUT_TOKEN` | `{"dogs": ["nanuq","anori"]}` (empty = nobody) |

**DB tables**: `dropbox_state(cursor)`, `runs(id, dropbox_path, content_hash UNIQUE, endurain_activity_id, sport, start_time, end_time, distance_m, has_gps, status, error)`, `companion_claims(id, received_at, dogs, run_id NULL)`, `dog_copies(run_id, dog, endurain_activity_id, status, UNIQUE(run_id, dog))`.

### 3.3 Matching companions to runs

The Shortcut fires when the Apple Watch workout ends, the FIT file arrives minutes later. Rule:
- A claim matches the **run whose `end_time` is closest to `received_at`**, within `−10 min … +3 h` (claim may come before the FIT arrives; it stays pending until then).
- A newer claim for the same run **replaces** the older one → that is the correction mechanism (re-run the Shortcut).
- Only running sports with GPS get copies. Claims that match nothing for 24 h are marked expired.

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
- Wake → WiFi → `GET /display.bin` with stored ETag (RTC memory) → on `200` full refresh, on `304` nothing → deep sleep 30 min (06:00–23:00), until 06:00 at night.
- WiFi credentials + API URL in a gitignored `secrets.h` (`secrets.example.h` committed).
- Deep sleep needs GPIO16 → RST; to be verified on the board. Fallback: run on USB.
- Plain HTTP is not allowed on the public domain if Traefik forces HTTPS → use `BearSSL::WiFiClientSecure` with `setInsecure()` (public data, no secrets sent).

## 4. Quality

- Rust: `cargo fmt`, `clippy -D warnings`, unit tests (GPX conversion, matching, stats, renderer snapshots), one ignored-by-default integration test against a local Endurain.
- Firmware: `pio run` must compile.
- GitHub Actions: Rust checks + PlatformIO build.

## 5. Delivery: PRs in order

| # | Branch | Title |
|---|---|---|
| 1 | `docs/plan` | `docs: add implementation plan` |
| 2 | `feat/compose` | `feat(deploy): docker compose with endurain` + `docs/setup.md` |
| 3 | `feat/server-skeleton` | `feat(server): axum skeleton, config, migrations, dockerfile, ci` |
| 4 | `feat/renderer` | `feat(server): render display image and serve png/bin` |
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
