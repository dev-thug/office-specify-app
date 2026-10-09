-- 0002_password_auth.sql — deviation X1 (spec §1, §4.5)
-- Email+password member login replaces OIDC SSO. `users.oidc_issuer` /
-- `oidc_subject` remain nullable and unused. Password credentials live in a
-- dedicated global table (like `users`, no workspace_id) reachable only via
-- the pre-auth repository path.
BEGIN;

CREATE TABLE user_password_credentials (
    user_id UUID PRIMARY KEY,
    -- argon2id PHC string (m=19456, t=2, p=1). Raw passwords never stored.
    password_hash TEXT NOT NULL,
    -- Login throttle: 5 consecutive failures → locked_until = now()+15m.
    failed_attempts INTEGER NOT NULL DEFAULT 0 CHECK (failed_attempts >= 0),
    locked_until TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE user_password_credentials ADD CONSTRAINT fk_pw_user
    FOREIGN KEY (user_id) REFERENCES users (id) ON DELETE CASCADE;

COMMIT;
