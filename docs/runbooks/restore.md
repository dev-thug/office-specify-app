# Database restore

**Hold point:** the approved spec marks backup/restore gate O01 closed. No repository backup format, backup command, or completed restore drill exists. Treat this as a guarded recovery procedure, not evidence that production recovery is ready. Do not resume outbound sends until the restored data and external effects have been reconciled.

## Before restoring

1. Stop the API, worker, MCP server, and web processes through the service manager that owns them. The repository does not include a service manager or shutdown command. For local foreground processes, stop each with `Ctrl-C`.
2. Record the backup timestamp and the recovery target. Restore into a fresh isolated database when possible; keep the original database and backup unchanged.
3. Use the matching PostgreSQL restore tool for the actual backup format. The repository does not generate a particular format. For a custom-format PostgreSQL archive, the command shape is:

   ```sh
   pg_restore --exit-on-error --dbname "$RESTORE_DATABASE_URL" "$BACKUP_FILE"
   ```

   For a plain SQL dump, use:

   ```sh
   psql "$RESTORE_DATABASE_URL" -v ON_ERROR_STOP=1 -f "$BACKUP_FILE"
   ```

   Verify PostgreSQL globals, runtime roles, extensions, ownership, and the migration version from the backup procedure before starting any Outreach OS process. The application does not migrate on startup. The CLI command `cargo run -p cli -- migrate` applies pending migrations; do not run it against a restored database until the schema/data recovery plan calls for that migration.

## Lock outbound submission

With the restored database selected in `RESTORE_DATABASE_URL`, use an owner connection to set the workspace flag before starting services:

```sh
psql "$RESTORE_DATABASE_URL" -v ON_ERROR_STOP=1 <<'SQL'
BEGIN;
UPDATE workspaces
   SET settings = jsonb_set(settings, '{send_workers_locked}', 'true'::jsonb, true);
COMMIT;
SQL
```

Verify every workspace is locked:

```sh
psql "$RESTORE_DATABASE_URL" -v ON_ERROR_STOP=1 -c \
  "SELECT id, name, settings->>'send_workers_locked' AS send_workers_locked FROM workspaces ORDER BY name, id;"
```

If any workspace is not `true`, keep all application processes stopped and correct the lock before proceeding. The API refuses new submissions while locked, and the worker rechecks the flag before crossing the provider submission boundary; queued dispatches are deferred without creating a send attempt. The restore lock is separate from `emergency_stop`; preserve both values during recovery.

## Reconcile before normal work resumes

1. Start the API and worker only after confirming that the deployed build contains the restore guard. While the lock remains set, the worker may handle other job types and defers outbound dispatches.
2. For each workspace, compare all post-backup external mail submissions and calendar changes against restored records. A database rollback does not undo provider-side mail or calendar effects.
3. Keep outbound rows with uncertain provider outcomes at `unknown`. Use the [unknown-send reconciliation runbook](unknown-send-reconciliation.md); never replay an uncertain send to “catch up.”
4. Check queued/running send jobs, approvals, suppressions, bookings, and campaigns against the provider-side record and restore point. Resolve stale work through the approved application workflow; do not delete jobs or overwrite status columns to make the restored database look current.
5. Record which workspace IDs and external records were reviewed, what remains uncertain, and the operator/change reference.

## Release the lock

After reconciliation is complete and an Admin has reviewed the evidence, release the lock through the audited API action. The session must belong to a human Admin. The action is workspace-scoped, so repeat it once for each restored workspace. Obtain `COOKIE_JAR` and `CSRF_TOKEN` from an authenticated Admin session for that workspace; the reason is hashed before it is written to audit metadata.

```sh
API_ORIGIN=${API_ORIGIN:-http://127.0.0.1:8080}
curl --fail-with-body --silent --show-error \
  --request POST "$API_ORIGIN/api/v1/operations/send-workers-lock/release" \
  --cookie "$COOKIE_JAR" \
  --header "X-CSRF-Token: $CSRF_TOKEN" \
  --header "Idempotency-Key: restore-reconcile-$(uuidgen)" \
  --header "Content-Type: application/json" \
  --data '{"reason":"Provider and calendar effects reconciled for restore incident <reference>"}'
```

The endpoint records the Admin, reason hash and length in the append-only audit log and emits an outbox event. Verify the returned `send_workers_locked` value is `false`, then verify the workspace setting from the restored database before allowing normal send traffic. Do not replace this API action with direct SQL; direct writes would bypass its authorization, idempotency, and audit behavior.

After a completed restore drill, update this runbook with the actual backup/restore command, roles/extensions procedure, evidence checks, audited lock release, and validation results. O01 remains closed until that drill is recorded.
