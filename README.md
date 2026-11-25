# running-viz 🏃🐻‍❄️🐻‍❄️

A monthly running challenge on an e-paper display: **200 km for me, 100 km each for Nanuq and Anori**, every month.

```
Apple Watch ─▶ HealthFit ─▶ Dropbox (FIT)
                                │
                     running-viz server (Rust)
                     ├─ uploads runs to Endurain
                     ├─ copies runs to the dogs' accounts
                     │   (GPX only: no heart rate, no cadence)
                     └─ renders a 400×300 1-bit overview
                                │
              ┌─────────────────┴─────────────────┐
     e-paper display (ESP8266)          browser (fullscreen)
```

After every run, an Apple Watch–triggered Shortcut asks **"Who came along?"** and tells the server which dogs ran with me.

## Hardware

| Part | Details |
|---|---|
| Display | Waveshare 4.2" e-paper raw panel, 400×300 px, black/white, SPI, partial refresh |
| Controller | Waveshare Universal e-Paper Driver Board with ESP8266 (WiFi), 24-pin FPC connector for raw panels |
| Power | battery; the ESP8266 wakes every 30 min, fetches the image and goes back to deep sleep (e-paper keeps the image without power) |
| Spare | Waveshare 2.7" e-paper raw panel, 264×176 px (same driver board) |

The display only shows a finished image: all logic and rendering happens on the server, the firmware just downloads a 15 000-byte frame (`/display.bin`) and draws it.

## Components

| Path | What | Tech |
|---|---|---|
| `server/` | Import, dog copies, rendering, API, optional SparkyFitness mirror | Rust, Postgres |
| `web/` | Fullscreen view of the rendered image | static web |
| `firmware/` | Fetches the image and draws it on the panel ([details](firmware/README.md)) | C++ / Arduino (ESP8266), PlatformIO |
| `deploy/` | Endurain + server + web | Docker Compose, Dokploy |

## Domains

| Service | URL |
|---|---|
| API | `running-viz-api.soeenplop.dev` |
| Frontend | `running-viz.soeenplop.dev` |
| Endurain | `endurain.soeenplop.dev` |
| SparkyFitness (trial) | `sparky.soeenplop.dev` |

## Getting started

See [docs/setup.md](docs/setup.md): start Endurain with Docker Compose and create all accounts, goals and API keys with one script.

## Development

```sh
cd server
cargo test                      # unit tests
TEST_DATABASE_URL=postgres://… cargo test   # + database tests
```

The server runs migrations on startup and exposes `GET /healthz` (database, Endurain sync and Dropbox import status).

Tests that read real HealthFit exports need `RUNNING_VIZ_FIT_FIXTURES=<dir with run.fit, treadmill.fit>`; those files contain GPS tracks and are not committed.

## Status

🚧 Work in progress. See [docs/PLAN.md](docs/PLAN.md) for the roadmap.
