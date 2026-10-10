BEGIN;

CREATE FUNCTION guard_smtp_backed_delivery_channel() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.provider = 'delivery_provider'
       AND EXISTS (
           SELECT 1 FROM smtp_connections sc
            WHERE sc.workspace_id = NEW.workspace_id
              AND sc.id = NEW.connection_id
       ) THEN
        RAISE EXCEPTION 'SMTP integrations are bound to their own profile channel'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER delivery_channel_no_smtp_clone
    BEFORE INSERT OR UPDATE OF connection_id, provider ON delivery_channels
    FOR EACH ROW EXECUTE FUNCTION guard_smtp_backed_delivery_channel();

COMMIT;
