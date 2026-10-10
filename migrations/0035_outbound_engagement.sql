-- Recipient-bound open/click request capabilities for immutable outbounds.
-- Public requests resolve only a token hash; event storage keeps one bounded
-- UTC-day counter per capability and never records IP or user-agent values.

BEGIN;

-- `expires_at` ends measurement; click redirects retain their fixed target
-- so older sent messages do not acquire broken content links.
CREATE TABLE outbound_engagement_capabilities (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL,
    outbound_message_id UUID NOT NULL,
    recipient_email_address_id UUID NOT NULL,
    capability_type TEXT NOT NULL CHECK (capability_type IN ('open', 'click')),
    token_hash BYTEA NOT NULL UNIQUE CHECK (octet_length(token_hash) = 32),
    redirect_target TEXT,
    expires_at TIMESTAMPTZ NOT NULL DEFAULT (now() + interval '30 days'),
    revoked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, id),
    UNIQUE (workspace_id, outbound_message_id, id),
    CHECK (
        (capability_type = 'open' AND redirect_target IS NULL)
        OR (capability_type = 'click' AND redirect_target ~* '^https?://')
    ),
    FOREIGN KEY (workspace_id, recipient_email_address_id, outbound_message_id)
        REFERENCES outbound_messages (workspace_id, to_email_address_id, id) ON DELETE CASCADE
);

CREATE INDEX outbound_engagement_capabilities_outbound_idx
    ON outbound_engagement_capabilities (workspace_id, outbound_message_id, capability_type);

CREATE TABLE outbound_engagement_request_buckets (
    workspace_id UUID NOT NULL,
    capability_id UUID NOT NULL,
    request_day DATE NOT NULL,
    request_count INTEGER NOT NULL DEFAULT 1 CHECK (request_count BETWEEN 1 AND 1000),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (workspace_id, capability_id, request_day),
    FOREIGN KEY (workspace_id, capability_id)
        REFERENCES outbound_engagement_capabilities (workspace_id, id) ON DELETE CASCADE
);

CREATE FUNCTION resolve_outbound_engagement_capability(p_token_hash BYTEA)
RETURNS TABLE(workspace_id UUID, capability_id UUID)
LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog, public AS $$
    SELECT c.workspace_id, c.id
      FROM public.outbound_engagement_capabilities c
     WHERE c.token_hash = p_token_hash
       AND c.revoked_at IS NULL
$$;

REVOKE ALL ON FUNCTION resolve_outbound_engagement_capability(BYTEA) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION resolve_outbound_engagement_capability(BYTEA) TO api_runtime;

ALTER TABLE outbound_engagement_capabilities ENABLE ROW LEVEL SECURITY;
ALTER TABLE outbound_engagement_capabilities FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON outbound_engagement_capabilities
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());

ALTER TABLE outbound_engagement_request_buckets ENABLE ROW LEVEL SECURITY;
ALTER TABLE outbound_engagement_request_buckets FORCE ROW LEVEL SECURITY;
CREATE POLICY workspace_isolation ON outbound_engagement_request_buckets
    USING (workspace_id = app_workspace_id())
    WITH CHECK (workspace_id = app_workspace_id());

GRANT SELECT, INSERT ON outbound_engagement_capabilities TO api_runtime;
GRANT SELECT, INSERT, UPDATE ON outbound_engagement_request_buckets TO api_runtime;
GRANT DELETE ON outbound_engagement_capabilities, outbound_engagement_request_buckets
    TO retention_operator;
GRANT SELECT (workspace_id, outbound_message_id)
    ON outbound_engagement_capabilities TO retention_operator;

COMMIT;
