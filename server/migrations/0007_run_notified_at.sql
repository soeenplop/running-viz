-- runs announced by push ("Wer war dabei?"), so each run is announced once
ALTER TABLE runs ADD COLUMN notified_at TIMESTAMPTZ;
