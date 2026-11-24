-- Dropbox sync position (single row).
CREATE TABLE dropbox_state (
    id      BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (id),
    cursor  TEXT NOT NULL
);

-- Every FIT file seen in Dropbox, with its bytes (needed for late dog copies).
CREATE TABLE fit_files (
    id            UUID PRIMARY KEY,
    dropbox_path  TEXT NOT NULL,
    content_hash  TEXT NOT NULL UNIQUE,
    bytes         BYTEA NOT NULL,
    received_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- My activities in Endurain (one per FIT file once uploaded).
CREATE TABLE runs (
    id                    UUID PRIMARY KEY,
    fit_file_id           UUID NOT NULL UNIQUE REFERENCES fit_files (id),
    endurain_activity_id  TEXT NOT NULL,
    sport                 TEXT NOT NULL,
    start_time            TIMESTAMPTZ NOT NULL,
    end_time              TIMESTAMPTZ NOT NULL,
    distance_m            INTEGER NOT NULL,
    has_gps               BOOLEAN NOT NULL
);
CREATE INDEX runs_end_time ON runs (end_time);

-- Resumable uploads: originals (my account) and copies (dog accounts).
CREATE TABLE uploads (
    id                    UUID PRIMARY KEY,
    kind                  TEXT NOT NULL CHECK (kind IN ('original', 'copy')),
    fit_file_id           UUID NOT NULL REFERENCES fit_files (id),
    run_id                UUID REFERENCES runs (id),
    dog                   TEXT,
    generation            INTEGER NOT NULL DEFAULT 0,
    idempotency_key       TEXT NOT NULL UNIQUE,
    job_id                TEXT,
    endurain_activity_id  TEXT,
    status                TEXT NOT NULL DEFAULT 'pending'
                          CHECK (status IN ('pending', 'job_started', 'uploaded', 'done',
                                            'delete_pending', 'deleted', 'needs_attention')),
    attempts              INTEGER NOT NULL DEFAULT 0,
    last_error            TEXT,
    raw_job               JSONB,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((kind = 'original') = (dog IS NULL)),
    CHECK (kind <> 'copy' OR run_id IS NOT NULL),
    -- a resumable state always carries the id needed to resume
    CHECK (status <> 'job_started' OR job_id IS NOT NULL),
    CHECK (status NOT IN ('uploaded', 'done', 'delete_pending') OR endurain_activity_id IS NOT NULL)
);
CREATE INDEX uploads_status ON uploads (status);

-- "Who came along?" answers from the Shortcut.
CREATE TABLE companion_claims (
    id           UUID PRIMARY KEY,
    received_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    ended_at     TIMESTAMPTZ,
    dogs         TEXT[] NOT NULL,
    run_id       UUID REFERENCES runs (id),
    status       TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'matched', 'expired')),
    CHECK (status <> 'matched' OR run_id IS NOT NULL)
);
CREATE INDEX companion_claims_status ON companion_claims (status);
