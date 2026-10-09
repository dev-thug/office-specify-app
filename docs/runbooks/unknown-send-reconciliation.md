# Unknown send reconciliation

Use this when an outbound message is `unknown` after the provider submission boundary. An unknown result means the request may have reached the provider. Never submit it again just because the response is missing.

## Automatic path

The worker marks the outbound and send attempt `unknown`, then queues `sending.reconcile_unknown` for five minutes later. Reconciliation calls only the provider's `lookup_sent` method using the outbound UUID as the correlation ID. A positive provider match records `accepted` with the provider message ID. No match leaves the outbound `unknown` and queues another lookup, up to twelve reconciliation jobs. It never resubmits the message. Provider lookup errors follow the worker's safe read retry policy.

Keep `outreach-worker` running with the same database configuration used by the API. There is no CLI command to force reconciliation. The application retry use case rejects an unknown external-effect job with `PROVIDER_SUBMISSION_OUTCOME_UNKNOWN`; do not edit the job or outbound rows to bypass that guard. An accepted outcome remains `accepted`, not `delivered`; delivery is recorded only from provider-confirmed evidence.

## Inspect a workspace

Use a database connection with read access to the workspace. These queries set the workspace RLS context and do not read message bodies or recipient snapshots. Set `DATABASE_URL`, `WORKSPACE_ID`, and, for the second query, `OUTBOUND_ID` in the shell first.

List unknown outbounds:

```sh
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -v workspace_id="$WORKSPACE_ID" <<'SQL'
BEGIN READ ONLY;
SELECT set_config('app.workspace_id', :'workspace_id', true);
SELECT id, purpose, status, failure_code, provider_message_id, created_at
  FROM outbound_messages
 WHERE workspace_id = :'workspace_id'::uuid
   AND status = 'unknown'
 ORDER BY created_at DESC;
ROLLBACK;
SQL
```

Inspect attempts and reconciliation jobs for one outbound:

```sh
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 \
  -v workspace_id="$WORKSPACE_ID" -v outbound_id="$OUTBOUND_ID" <<'SQL'
BEGIN READ ONLY;
SELECT set_config('app.workspace_id', :'workspace_id', true);
SELECT attempt_no, status, provider_request_id, provider_message_id,
       error_code, started_at, finished_at
  FROM send_attempts
 WHERE workspace_id = :'workspace_id'::uuid
   AND outbound_message_id = :'outbound_id'::uuid
 ORDER BY attempt_no;
SELECT id, state, attempt_count, available_at, last_error_code, created_at
  FROM jobs
 WHERE workspace_id = :'workspace_id'::uuid
   AND kind = 'sending.reconcile_unknown'
   AND payload->>'outbound_message_id' = :'outbound_id'
 ORDER BY created_at;
ROLLBACK;
SQL
```

## If evidence is still missing

Confirm the outbound remains `unknown` and check the worker logs for lookup errors. When using a real provider adapter, compare the provider's sent-item or submission record using the outbound UUID/correlation header. A missing match is not proof that the message was not accepted. Do not retry or create a replacement message until a human has resolved the provider-side record and the recipient impact.

If the bounded lookup chain is exhausted, or its job reaches `dead`, preserve the unknown status and escalate with the outbound ID, attempt rows, job IDs, and provider-side evidence. The current repository has no supported operator command or API action to force a reconciliation or manually resolve an unknown send. Do not write a guessed provider ID, mark the outbound delivered, or requeue the send job.

## Fake-provider limits

Local and current worker execution use process-local fakes. A scripted `timeout`/`unknown` outcome does not create a fake sent record, so lookup finds no evidence. A worker restart also discards fake lookup history. The T13 test uses a controllable provider stub to simulate acceptance followed by a lost response; it does not validate a real provider's lookup behavior or cross-restart persistence. Current worker code does not wire a real provider adapter, so production reconciliation is not enabled.
