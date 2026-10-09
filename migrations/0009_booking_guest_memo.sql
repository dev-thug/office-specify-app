-- Optional guest-supplied booking context, with an explicit storage limit.
ALTER TABLE bookings ADD COLUMN guest_memo TEXT
    CHECK (guest_memo IS NULL OR char_length(guest_memo) <= 2000);
