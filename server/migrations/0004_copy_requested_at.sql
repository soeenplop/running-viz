-- Copies: receipt time of the newest claim that wanted them. A deleted copy (by us or
-- removed by hand in Endurain) is only re-created for a claim received after that.
ALTER TABLE uploads ADD COLUMN requested_at TIMESTAMPTZ;
