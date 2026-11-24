-- The Shortcut sends the workout's start and end; matching needs both.
ALTER TABLE companion_claims ADD COLUMN started_at TIMESTAMPTZ;
CREATE INDEX runs_start_time ON runs (start_time);
-- Set when the original was deleted in Endurain; its dog copies are removed.
ALTER TABLE runs ADD COLUMN gone BOOLEAN NOT NULL DEFAULT FALSE;
