-- Job title shown under the host name on public booking pages.
ALTER TABLE principals
    ADD COLUMN title TEXT CHECK (title IS NULL OR char_length(title) <= 100);
