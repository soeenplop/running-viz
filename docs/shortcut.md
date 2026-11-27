# "Who came along?"

There are two ways to tell the server which dogs came along: the **dogs page** (recommended) and an iOS **Shortcut**. Both use the same token (`SHORTCUT_TOKEN`).

## The dogs page

Open `https://running-viz-api.soeenplop.dev/dogs#<SHORTCUT_TOKEN>` once on the iPhone and add it to the home screen. The token stays in the URL fragment, which the browser never sends to the server; the page also remembers it on the device.

The page lists the newest 20 runs. Unanswered outdoor runs are highlighted. Tap **Nanuq**, **Anori** or **Keiner**; every tap is saved right away and is the newest answer for that run, so it also corrects older runs and earlier Shortcut answers. Treadmill runs and runs without GPS are shown but count only for me.

API behind it (both need `Authorization: Bearer <token>`):

- `GET /api/runs` → `{"dogs": [...], "runs": [{"id", "start_time", "end_time", "distance_m", "eligible", "dogs"}]}`; `dogs` is `null` while a run has no answer.
- `PUT /api/runs/{id}/dogs` with `{"dogs": ["nanuq"]}` → `200 {"status":"saved","dogs":[...]}`, `404` for an unknown run.

## Push after each run

With `NTFY_URL` set (e.g. `https://ntfy.sh/<random topic>`), the server sends a push via [ntfy](https://ntfy.sh) for every new outdoor run with GPS that has no answer yet (once per run, only for runs that ended in the last 24 hours):

> **Wer war dabei?** Lauf Fr 27.11., 08:47 · 5,4 km — [Beide] [Nanuq] [Anori]

- A button saves the answer right away (like the page) and clears the push. Nobody came: just swipe it away.
- Tapping the message opens the dogs page (`PUBLIC_URL/dogs`).
- The buttons don't carry the token: each link has a signature (`?sig=`, HMAC of the run id with `SHORTCUT_TOKEN`) that is valid for that run only. The topic name is the only secret ntfy.sh sees; anyone who knows it can read the run times, so make it random.

On the iPhone: install the ntfy app and subscribe to the topic (server `ntfy.sh`, topic = the part after the last `/`).

## The Shortcut

After every workout the Apple Watch can trigger an iOS Shortcut that asks which dogs came along and tells the server. The server attaches the answer to the matching run and copies that run (GPS track only, no heart rate, cadence or power) into Nanuq's and Anori's Endurain accounts.

### Server side

`deploy/.env`:

```sh
SHORTCUT_TOKEN=$(openssl rand -hex 24)   # paste the value, keep it secret
```

Endpoint: `POST https://running-viz-api.soeenplop.dev/api/companions`

```http
Authorization: Bearer <SHORTCUT_TOKEN>
Content-Type: application/json

{"dogs": ["nanuq", "anori"], "started_at": "2026-12-01T07:00:00+01:00", "ended_at": "2026-12-01T07:31:12+01:00"}
```

- `dogs`: any of `nanuq`, `anori`. An empty list means nobody came.
- `started_at` / `ended_at`: start and end of the workout (ISO 8601). The server only matches the run whose start **and** end are within ±2 minutes. Send both or neither: only a start, or a start after the end, is rejected (`400`). Without times, the server uses the run that ended between 5 minutes before and 1 minute after the request.
- At most one answer per minute (`429` otherwise). An answer that cannot be matched within 24 hours expires.
- The answer is `202 {"status":"received","dogs":["anori","nanuq"]}`: the dogs as saved (lower case, sorted, without duplicates), so the Shortcut can show them. The copies follow within seconds, at the latest on the next 5-minute sync.

### Setup

1. **Automation** (Shortcuts app → Automation → New): trigger *Apple Watch Workout*, *ends*, any workout type. Run immediately, no confirmation.
2. Actions:
   1. *Find Health Samples* (Gesundheitsproben suchen) or *Find Workouts* (depends on iOS version): latest workout, sorted by end date, limit 1. Take its **start date** and **end date**.
   2. *Choose from List* (Aus Liste auswählen), with *Select Multiple* on: `Nanuq`, `Anori`. The prompt shows the workout's date and time, e.g. „Lauf Di 01.12., 07:00 – wer war dabei?“, so you always see which run you answer for. Selecting nothing means nobody.
   3. *Get Contents of URL*: `POST` to the endpoint above, header `Authorization: Bearer <token>`, JSON body `dogs` (the selection, lower case), `started_at`, `ended_at` (dates formatted as ISO 8601).
   4. *Show Notification*: „Gespeichert ✓“ plus the response's `dogs` (*Get Dictionary Value* `dogs`), so you see what the server saved.

If your iOS version can't read the workout's dates, leave `started_at`/`ended_at` out. The server then uses the time window described above (5 minutes before to 1 minute after the request). In this mode, **all corrections happen by hand in Endurain**: running the Shortcut again later would send a new time and could hit a different run.

## Corrections

- **Any run** on the dogs page: the newest answer wins. Dogs that are no longer selected lose their copy, newly selected dogs get one.
- **In Endurain:** a dog's copy you delete there stays deleted, unless a new answer for that run includes the dog again.
- If you delete one of **your** runs in Endurain, the server removes its dog copies within a day.

Copies are only made for outdoor runs with GPS (`run`, `trail_run`, `track_run`). Treadmill runs never get copies.
