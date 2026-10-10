BEGIN;

-- Stable RFC Message-ID generated when an outbound snapshot is frozen.
-- The existing outbound snapshot trigger makes this field immutable after
-- insertion; the scoped index is the correlation key used by inbox ingest.
ALTER TABLE outbound_messages
    ADD COLUMN internet_message_id TEXT,
    ADD CONSTRAINT outbound_internet_message_id_shape CHECK (
        internet_message_id IS NULL OR (
            length(internet_message_id) BETWEEN 5 AND 998
            AND internet_message_id ~ '^<[^<>[:space:]@]+@[^<>[:space:]@]+>$'
        )
    );

CREATE UNIQUE INDEX outbound_internet_message_id_uq
    ON outbound_messages (workspace_id, mail_account_id, internet_message_id)
    WHERE internet_message_id IS NOT NULL;

-- The local sent-message projection is written after the provider reports
-- accepted/unknown. One outbound may have at most one inbox message row.
CREATE UNIQUE INDEX email_messages_outbound_mirror_uq
    ON email_messages (workspace_id, outbound_message_id)
    WHERE direction = 'outbound' AND outbound_message_id IS NOT NULL;

COMMIT;
