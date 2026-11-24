# AGENTS.md

Monthly running challenge for one human and two dogs (Nanuq, Anori), shown on an e-paper display and in the browser.

## Layout

- `server/`: Rust API (Dropbox → Endurain import, dog copies, rendering)
- `web/`: fullscreen browser view of the rendered image
- `firmware/`: ESP8266 + Waveshare 4.2" e-paper (C++/Arduino, PlatformIO)
- `deploy/`: Docker Compose (Endurain, Postgres, server, web), Dokploy

## Rules

- Everything in English: code, comments, docs, commits, PRs.
- Conventional Commits (`feat:`, `fix:`, `chore:`, `docs:` …), also for PR titles.
- One feature per branch (`feat/<name>`) and one PR per feature. Never commit straight to `main`.
- PRs are reviewed by GPT-6.2 Astra. Discussion happens in PR comments. Merge with a merge commit (no squash, no rebase; keeps every commit) only after a comment marked `✅ Approved by GPT-6.2 Astra`.
- Keep the README up to date when behavior or setup changes.
- No secrets in the repo; use `.env` (gitignored) and document variables in `.env.example`.
- Keep it simple: the display must be readable at a glance without explanation.
