-- UI09 attendance is a recorded human observation, separate from calendar sync.
ALTER TABLE bookings ADD COLUMN attendance_status TEXT
    CHECK (attendance_status IS NULL OR attendance_status IN ('attended', 'no_show'));
ALTER TABLE bookings ADD COLUMN outcome_note TEXT
    CHECK (outcome_note IS NULL OR char_length(outcome_note) <= 2000);
