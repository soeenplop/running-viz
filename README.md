# running-viz 🏃🐕🐕

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

## Components

| Path | What | Tech |
|---|---|---|
| `server/` | Import, dog copies, rendering, API | Rust, Postgres |
| `web/` | Fullscreen view of the rendered image | static web |
| `firmware/` | Fetches the image and draws it on the panel | C++ / Arduino (ESP8266) |
| `deploy/` | Endurain + server + web | Docker Compose, Dokploy |

## Domains

| Service | URL |
|---|---|
| API | `running-api.soeenplop.dev` |
| Frontend | `running.soeenplop.dev` |
| Endurain | `endurain.soeenplop.dev` |

## Getting started

See [docs/setup.md](docs/setup.md): start Endurain with Docker Compose and create all accounts, goals and API keys with one script.

## Development

```sh
cd server
cargo test                      # unit tests
TEST_DATABASE_URL=postgres://… cargo test   # + database tests
```

The server runs migrations on startup and exposes `GET /healthz`.

## Status

🚧 Work in progress. See [docs/PLAN.md](docs/PLAN.md) for the roadmap.
