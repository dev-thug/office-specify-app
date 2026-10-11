ALTER TABLE contacts ADD COLUMN phone TEXT
    CHECK (phone IS NULL OR char_length(phone) <= 80);
