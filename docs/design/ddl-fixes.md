# DDL fixes — appendix A.4 vs PostgreSQL 17.11

Migration `0001_schema.sql` is the appendix A.4 DDL (design doc lines
~1088–3334) copied verbatim. It applied to an empty PostgreSQL 17.11
database with **zero errors and zero warnings**, so no syntax/order fixes
were required.

| # | Source line | Exact error | Change | Why intent preserved | Class |
|---|-------------|-------------|--------|----------------------|-------|
| — | — | — | none | — | — |

**Total fixes: 0. Semantic fixes: 0.**

## Notes (not fixes — verified or decided behavior)

- `CREATE EXTENSION btree_gist` is present and sufficient for the
  `booking_slot_holds` EXCLUDE constraint on `tstzrange`.
- `gen_random_uuid()` is built in on PG17 — no `pgcrypto` needed by the DDL.
- A.4 already declares `ENABLE ROW LEVEL SECURITY`, `FORCE ROW LEVEL
  SECURITY` and `CREATE POLICY workspace_isolation ... USING (workspace_id =
  app_workspace_id())` on all workspace-scoped tables. `app_workspace_id()`
  returns NULL when `app.workspace_id` is unset → runtime roles see 0 rows
  unless `Db::begin` sets it (verified: cross-workspace INSERT rejected).
- `jobs.dedupe_key` is `NOT NULL` with `UNIQUE (workspace_id, kind,
  dedupe_key)`. The shared contract is `enqueue(..., dedupe_key:
  Option<&str>)`; `None` maps to a generated `"once:<uuid>"` key so it can
  never collide — same semantics as SQL NULL (always distinct) without
  relaxing the schema.
- Roles/grants/guard triggers per A.5/A.6.2 live in
  `0003_rls_roles_guards.sql` (A.5 explicitly defers role creation to a
  deployment step). Append-only enforcement on `audit_logs`,
  `approval_decisions`, `suppression_events` is via REVOKE + guard triggers
  gated on `app.retention_operator='on'`; `send_attempts` forbids DELETE and
  UPDATE once `finished_at` is set.
- `users`, `user_password_credentials`, `workspaces` carry no
  `workspace_id` and no RLS — intentional per A.5 (pre-auth lookups go
  through the dedicated auth repository only).
- Deviation X1 adds `user_password_credentials` in `0002_password_auth.sql`
  (FK `user_id → users.id ON DELETE CASCADE`).
- `0004_scan_functions.sql` (addition, not an A.4 fix — **semantic**
  companion): FORCE RLS means the worker runner cannot enumerate
  "workspaces with pending jobs/outbox" — with `app.workspace_id` unset the
  policies match zero rows for every role including the table owner, and
  A.4 forbids BYPASSRLS on runtime accounts. The migration adds three
  `SECURITY DEFINER` functions owned by the migration superuser (which
  bypasses RLS): `runnable_job_workspaces(kinds)` (kind-scoped since 0007 —
  a runner never sees jobs it has no handler for), `pending_outbox_workspaces()`
  and `reclaim_expired_job_leases()`. They expose only workspace ids / a
  row count; all row access still happens inside per-workspace RLS
  transactions. EXECUTE is revoked from PUBLIC and granted to
  `api_runtime`/`worker_runtime` only.
