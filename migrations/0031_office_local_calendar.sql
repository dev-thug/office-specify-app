BEGIN;

-- Office's local calendar is its own integration purpose. Zoho mail and
-- delivery-purpose checks remain provider-specific and unchanged.
ALTER TABLE integration_connections
    DROP CONSTRAINT integration_connections_provider_check;
ALTER TABLE integration_connections
    ADD CONSTRAINT integration_connections_provider_check
    CHECK (provider IN (
        'zoho_mail', 'zoho_calendar', 'delivery_provider',
        'transactional_provider', 'zoho_campaigns', 'office_calendar'
    ));
ALTER TABLE integration_connections
    ADD CONSTRAINT office_calendar_has_no_external_credentials
    CHECK (provider <> 'office_calendar' OR (region = 'local' AND secret_ref = ''));

ALTER TABLE calendar_connections
    DROP CONSTRAINT calendar_connections_provider_check;
ALTER TABLE calendar_connections
    ADD CONSTRAINT calendar_connections_provider_check
    CHECK (provider IN ('zoho_calendar', 'office_calendar'));
ALTER TABLE calendar_connections
    ADD CONSTRAINT office_calendar_id_is_bound_to_its_row
    CHECK (
        provider <> 'office_calendar'
        OR provider_calendar_id = 'office:' || workspace_id::text || ':' || id::text
    );

CREATE UNIQUE INDEX calendar_connections_office_one_per_host
    ON calendar_connections (workspace_id, host_principal_id)
    WHERE provider = 'office_calendar';

CREATE TABLE office_calendar_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    host_principal_id UUID NOT NULL,
    calendar_connection_id UUID NOT NULL,
    booking_id UUID NOT NULL,
    provider_event_id TEXT NOT NULL,
    title TEXT NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 200),
    attendee_email TEXT NOT NULL CHECK (length(attendee_email) BETWEEN 3 AND 320),
    starts_at TIMESTAMPTZ NOT NULL,
    ends_at TIMESTAMPTZ NOT NULL,
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'cancelled')),
    revision BIGINT NOT NULL DEFAULT 1 CHECK (revision > 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, booking_id),
    UNIQUE (workspace_id, provider_event_id),
    CHECK (ends_at > starts_at),
    CHECK (provider_event_id = 'office-event:' || booking_id::text),
    FOREIGN KEY (workspace_id, host_principal_id, calendar_connection_id)
        REFERENCES calendar_connections (workspace_id, host_principal_id, id),
    FOREIGN KEY (workspace_id, host_principal_id, booking_id)
        REFERENCES bookings (workspace_id, host_principal_id, id)
);

CREATE INDEX office_calendar_events_active_range
    ON office_calendar_events (workspace_id, calendar_connection_id, starts_at)
    WHERE status = 'active';

ALTER TABLE office_calendar_events
    ADD CONSTRAINT office_calendar_events_no_host_overlap
    EXCLUDE USING gist (
        workspace_id WITH =,
        host_principal_id WITH =,
        tstzrange(starts_at, ends_at, '[)') WITH &&
    ) WHERE (status = 'active')
    DEFERRABLE INITIALLY IMMEDIATE;

CREATE FUNCTION guard_office_calendar_event_binding() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE' AND (
        NEW.workspace_id IS DISTINCT FROM OLD.workspace_id
        OR NEW.host_principal_id IS DISTINCT FROM OLD.host_principal_id
        OR NEW.calendar_connection_id IS DISTINCT FROM OLD.calendar_connection_id
        OR NEW.booking_id IS DISTINCT FROM OLD.booking_id
        OR NEW.provider_event_id IS DISTINCT FROM OLD.provider_event_id
        OR NEW.created_at IS DISTINCT FROM OLD.created_at
    ) THEN
        RAISE EXCEPTION 'Office calendar event identity is immutable'
            USING ERRCODE = '23514';
    END IF;
    IF NOT EXISTS (
        SELECT 1
          FROM calendar_connections c
          JOIN integration_connections i
            ON i.workspace_id = c.workspace_id AND i.id = c.connection_id
         WHERE c.workspace_id = NEW.workspace_id
           AND c.id = NEW.calendar_connection_id
           AND c.host_principal_id = NEW.host_principal_id
           AND c.provider = 'office_calendar'
           AND c.status = 'active'
           AND i.provider = 'office_calendar'
           AND i.status = 'active'
    ) OR NOT EXISTS (
        SELECT 1
          FROM bookings b
          JOIN booking_links l
            ON l.workspace_id = b.workspace_id AND l.id = b.booking_link_id
          JOIN meeting_types m
            ON m.workspace_id = l.workspace_id AND m.id = l.meeting_type_id
          JOIN email_addresses a
            ON a.workspace_id = b.workspace_id AND a.id = b.guest_email_address_id
         WHERE b.workspace_id = NEW.workspace_id
           AND b.id = NEW.booking_id
           AND b.host_principal_id = NEW.host_principal_id
           AND m.calendar_connection_id = NEW.calendar_connection_id
           AND lower(a.address_original) = lower(NEW.attendee_email)
           AND b.status IN ('confirming', 'confirmed', 'canceling', 'unknown', 'canceled')
    ) THEN
        RAISE EXCEPTION 'Office calendar event must bind an active host calendar and its booking'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER office_calendar_event_binding_guard
    BEFORE INSERT OR UPDATE ON office_calendar_events
    FOR EACH ROW EXECUTE FUNCTION guard_office_calendar_event_binding();

ALTER TABLE office_calendar_events ENABLE ROW LEVEL SECURITY;
ALTER TABLE office_calendar_events FORCE ROW LEVEL SECURITY;
CREATE POLICY office_calendar_host_workspace_scope ON office_calendar_events
    USING (
        workspace_id = app_workspace_id()
        AND EXISTS (
            SELECT 1 FROM calendar_connections c
             WHERE c.workspace_id = office_calendar_events.workspace_id
               AND c.id = office_calendar_events.calendar_connection_id
               AND c.host_principal_id = office_calendar_events.host_principal_id
               AND c.provider = 'office_calendar'
        )
    )
    WITH CHECK (
        workspace_id = app_workspace_id()
        AND EXISTS (
            SELECT 1 FROM calendar_connections c
             WHERE c.workspace_id = office_calendar_events.workspace_id
               AND c.id = office_calendar_events.calendar_connection_id
               AND c.host_principal_id = office_calendar_events.host_principal_id
               AND c.provider = 'office_calendar'
        )
    );

GRANT SELECT, INSERT, UPDATE ON office_calendar_events TO api_runtime, worker_runtime;
REVOKE ALL ON office_calendar_events FROM report_reader;

COMMIT;
