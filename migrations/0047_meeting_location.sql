-- How a meeting is held, shown to guests on the public booking pages:
-- online (video link, often sent after confirmation) or offline (address).
ALTER TABLE meeting_types
    ADD COLUMN location_kind TEXT NOT NULL DEFAULT 'online'
        CHECK (location_kind IN ('online', 'offline')),
    ADD COLUMN location_detail TEXT
        CHECK (location_detail IS NULL OR char_length(location_detail) <= 500);
