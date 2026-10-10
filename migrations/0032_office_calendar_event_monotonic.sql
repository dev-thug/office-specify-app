BEGIN;

-- Cancellation is terminal even for direct SQL writes by the runtime roles.
CREATE FUNCTION guard_office_calendar_event_status_monotonic() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE'
       AND OLD.status = 'cancelled'
       AND NEW.status <> 'cancelled' THEN
        RAISE EXCEPTION 'cancelled Office calendar events cannot be reactivated'
            USING ERRCODE = '23514';
    END IF;
    IF NEW.status = 'active'
       AND EXISTS (
           SELECT 1 FROM bookings b
            WHERE b.workspace_id = NEW.workspace_id
              AND b.id = NEW.booking_id
              AND b.status = 'canceled'
       ) THEN
        RAISE EXCEPTION 'cancelled bookings cannot have an active Office calendar event'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER office_calendar_event_status_monotonic
    BEFORE INSERT OR UPDATE OF status ON office_calendar_events
    FOR EACH ROW EXECUTE FUNCTION guard_office_calendar_event_status_monotonic();

COMMIT;
