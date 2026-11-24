-- Facts read from each FIT file, and why a file was not uploaded.
ALTER TABLE fit_files
    ADD COLUMN dropbox_id   TEXT,
    ADD COLUMN sport        TEXT,
    ADD COLUMN sub_sport    TEXT,
    ADD COLUMN start_time   TIMESTAMPTZ,
    ADD COLUMN end_time     TIMESTAMPTZ,
    ADD COLUMN distance_m   INTEGER,
    ADD COLUMN has_gps      BOOLEAN,
    -- NULL = uploaded; otherwise 'unreadable', 'before_since' or 'duplicate'
    ADD COLUMN skip_reason  TEXT CHECK (skip_reason IN ('unreadable', 'before_since', 'duplicate'));

CREATE INDEX fit_files_start_time ON fit_files (start_time);
