# Setup

## 1. Start Endurain

```sh
cd deploy
install -m 600 .env.example .env   # owner-only: it will hold passwords and API keys
# fill in the secrets (the commands are in the file)
# first start: only the database and Endurain (the server needs API keys, created in step 2)
docker compose up -d db endurain                                   # production
docker compose -f compose.yaml -f compose.dev.yaml up -d db endurain   # local: ports + OpenAPI docs
```

The first start creates one Postgres with two databases (`endurain`, `running_viz`).
Locally, Endurain runs at <http://localhost:8080>.

## 2. Accounts, goals and API keys

A fresh Endurain prints a one-time setup token:

```sh
docker compose logs endurain | grep setup_token
```

Then one script sets everything up: the admin (me), the dog accounts `nanuq` and `anori`, monthly run goals (200 / 100 / 100 km), privacy defaults for the dogs (visible to followers, heart rate/power/cadence hidden), and an API key per account.

```sh
# still in deploy/
ENDURAIN_URL=http://localhost:8080 \
SETUP_TOKEN='<token>' ADMIN_NAME='Sören' ADMIN_EMAIL='<email>' \
ADMIN_USERNAME=soeren ADMIN_PASSWORD='<password>' \
../scripts/endurain-bootstrap.sh >> .env
```

- Then start everything (same `-f` flags as above): `docker compose up -d`.
- The display shows each account's Endurain profile name (so `ADMIN_NAME` is what you see on the display).
- The script appends `ENDURAIN_KEY_SOEREN`, `ENDURAIN_KEY_NANUQ`, `ENDURAIN_KEY_ANORI` and the generated dog passwords to `.env`.
- It is safe to re-run, even after an interruption. Existing users and goals are kept, and each run creates new API keys (remove the old lines from `.env`). Pass `NANUQ_PASSWORD` / `ANORI_PASSWORD` to keep the dogs' passwords; otherwise, or if they don't work, the admin sets new ones and the script prints them.
- If a request fails, the script stops with a non-zero exit code and writes nothing for that step.
- To see the dogs' runs in your feed, follow `nanuq` and `anori` in the Endurain UI and accept the requests in their accounts.

## 3. HealthFit import (Dropbox)

HealthFit exports every workout as a FIT file to Dropbox (`/Apps/HealthFitExporter`). The server reads that folder.

1. Create an app in the [Dropbox App Console](https://www.dropbox.com/developers/apps):
   - access type **Full Dropbox** (an app-folder app cannot see HealthFit's folder);
   - under **Permissions**, enable `files.metadata.read` and `files.content.read` and click **Submit**.
2. Get a refresh token once. Open this URL with your app key, allow access, and copy the code:
   `https://www.dropbox.com/oauth2/authorize?client_id=<APP_KEY>&response_type=code&token_access_type=offline`
   ```sh
   curl https://api.dropboxapi.com/oauth2/token -d grant_type=authorization_code \
     -d code='<CODE>' -u '<APP_KEY>:<APP_SECRET>'
   ```
   The response must list both scopes under `scope`; otherwise submit the permissions and repeat.
3. Add to `deploy/.env`: `DROPBOX_APP_KEY`, `DROPBOX_APP_SECRET`, `DROPBOX_REFRESH_TOKEN`
   (optional: `DROPBOX_FOLDER`, default `/Apps/HealthFitExporter`; `DROPBOX_IMPORT_SINCE`, default `2026-11-01`).
4. Optional, after deployment: under **Webhooks** in the App Console, add
   `https://running-viz-api.soeenplop.dev/dropbox/webhook`. New runs then show up within seconds;
   without it the server checks every 5 minutes.

What the import does: every new FIT file is uploaded to my Endurain account. Files from before `DROPBOX_IMPORT_SINCE`, unreadable files, and second exports of the same workout (same start ±60 s) are skipped and remembered, so they are not checked again. Uploads that Endurain answers ambiguously are marked `needs_attention` in the `uploads` table and left alone.

## 4. API key renewal

Keys are valid for up to 10 years (the script raises the server limit). To rotate a key:
1. Create a new key in Endurain (Settings → Security → API keys).
2. Replace it in `deploy/.env`.
3. Restart the server with `docker compose up -d server`.
4. Revoke the old key.
