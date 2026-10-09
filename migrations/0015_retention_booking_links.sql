-- Personalized public links must be revoked when their contact is erased.
GRANT SELECT, UPDATE ON booking_links TO retention_operator;
