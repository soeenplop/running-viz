-- SparkyFitness mirror: what was last sent per account and day ('' = cleared).
CREATE TABLE sparky_days (
    account       TEXT NOT NULL,
    day           DATE NOT NULL,
    payload_hash  TEXT NOT NULL,
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (account, day)
);
