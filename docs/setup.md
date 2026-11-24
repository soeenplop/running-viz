# Setup

## 1. Start Endurain

```sh
cd deploy
install -m 600 .env.example .env   # owner-only: it will hold passwords and API keys
# fill in the secrets (the commands are in the file)
docker compose up -d                                   # production
docker compose -f compose.yaml -f compose.dev.yaml up -d   # local: ports + OpenAPI docs
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

- The display shows each account's Endurain profile name (so `ADMIN_NAME` is what you see on the display).
- The script appends `ENDURAIN_KEY_SOEREN`, `ENDURAIN_KEY_NANUQ`, `ENDURAIN_KEY_ANORI` and the generated dog passwords to `.env`.
- It is safe to re-run, even after an interruption. Existing users and goals are kept, and each run creates new API keys (remove the old lines from `.env`). Pass `NANUQ_PASSWORD` / `ANORI_PASSWORD` to keep the dogs' passwords; otherwise, or if they don't work, the admin sets new ones and the script prints them.
- If a request fails, the script stops with a non-zero exit code and writes nothing for that step.
- To see the dogs' runs in your feed, follow `nanuq` and `anori` in the Endurain UI and accept the requests in their accounts.

## 3. API key renewal

Keys are valid for up to 10 years (the script raises the server limit). To rotate a key:
1. Create a new key in Endurain (Settings → Security → API keys).
2. Replace it in `deploy/.env`.
3. Restart the server with `docker compose up -d server`.
4. Revoke the old key.
