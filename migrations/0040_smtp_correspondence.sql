BEGIN;

CREATE OR REPLACE FUNCTION guard_smtp_connection_binding() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM delivery_channels dc
         WHERE dc.workspace_id = NEW.workspace_id
           AND dc.id = NEW.channel_id
           AND dc.connection_id = NEW.id
           AND dc.mail_account_id = NEW.mail_account_id
           AND dc.sender_identity_id = NEW.sender_identity_id
           AND dc.provider = 'delivery_provider'
           AND dc.purpose IN ('marketing','correspondence')
    ) THEN
        RAISE EXCEPTION 'SMTP connection must bind its own Office channel with an allowed purpose'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE FUNCTION guard_smtp_channel_purpose_immutable() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.purpose IS DISTINCT FROM OLD.purpose
       AND EXISTS (
           SELECT 1 FROM smtp_connections sc
            WHERE sc.workspace_id = NEW.workspace_id
              AND sc.channel_id = NEW.id
       ) THEN
        RAISE EXCEPTION 'SMTP profile channel purpose is immutable'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER smtp_channel_purpose_immutable
    BEFORE UPDATE OF purpose ON delivery_channels
    FOR EACH ROW EXECUTE FUNCTION guard_smtp_channel_purpose_immutable();

COMMIT;
