-- Workers may recheck credential authority without reading token hashes.
CREATE FUNCTION credential_authorizes(
    credential UUID, principal UUID DEFAULT NULL, required_scopes TEXT[] DEFAULT '{}'
) RETURNS BOOLEAN
LANGUAGE sql STABLE SECURITY DEFINER
SET search_path = pg_catalog, public AS $$
    SELECT EXISTS (
        SELECT 1 FROM public.service_credentials c
         WHERE c.workspace_id = public.app_workspace_id()
           AND c.id = credential
           AND (principal IS NULL OR c.principal_id = principal)
           AND c.revoked_at IS NULL AND c.expires_at > now()
           AND c.scopes @> required_scopes
    )
$$;
REVOKE ALL ON FUNCTION credential_authorizes(UUID, UUID, TEXT[]) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION credential_authorizes(UUID, UUID, TEXT[])
    TO api_runtime, worker_runtime;
