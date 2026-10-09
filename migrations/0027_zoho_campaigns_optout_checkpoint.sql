-- Aggregate/status reads do not establish unsubscribe coverage. Only a
-- complete primary list read may advance this separate freshness checkpoint.
ALTER TABLE marketing_exports ADD COLUMN unsubscribe_checked_at TIMESTAMPTZ;
