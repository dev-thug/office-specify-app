-- A password change is global to users; revoke the same user's other
-- workspace sessions without exposing those rows to api_runtime.
CREATE FUNCTION revoke_other_user_sessions(account_user UUID, current_session UUID)
RETURNS BIGINT
LANGUAGE plpgsql SECURITY DEFINER
SET search_path = pg_catalog, public AS $$
DECLARE revoked_count BIGINT;
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM public.user_sessions s
        JOIN public.principals p ON p.workspace_id=s.workspace_id AND p.id=s.principal_id
        WHERE s.id=current_session AND s.workspace_id=public.app_workspace_id()
          AND s.revoked_at IS NULL AND s.expires_at>now()
          AND p.user_id=account_user AND p.kind='human' AND p.status='active'
    ) THEN
        RAISE EXCEPTION 'active self session required' USING ERRCODE='42501';
    END IF;
    UPDATE public.user_sessions s SET revoked_at=now()
      FROM public.principals p
     WHERE p.workspace_id=s.workspace_id AND p.id=s.principal_id
       AND p.user_id=account_user AND p.kind='human'
       AND s.id<>current_session AND s.revoked_at IS NULL;
    GET DIAGNOSTICS revoked_count = ROW_COUNT;
    RETURN revoked_count;
END;
$$;
REVOKE ALL ON FUNCTION revoke_other_user_sessions(UUID, UUID) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION revoke_other_user_sessions(UUID, UUID) TO api_runtime;
