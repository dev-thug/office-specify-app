# Cross-module integration contracts (binding for Wave B)

Modules never call each other's application functions in Wave B. They integrate through **(a) shared tables** (all exist from migration 0001),
**(b) durable jobs** (`jobs::enqueue`, handler owned by exactly one module) and **(c) outbox events** (`outbox::emit`, consumed via the
worker's `EventConsumer` registry with `processed_events` dedupe). Payloads carry IDs + versions only — never bodies, addresses or secrets (§16.2).
Every payload has `payload_version = 1`. Unknown fields must be ignored by consumers.

## Job kinds (`kind` → owner module → payload)

| kind | owner (handler) | enqueued by | payload | notes |
|---|---|---|---|---|
| `sending.dispatch_outbound` | sending | sending (single submit), campaigns (scheduler), approvals (outbound approved) | `{outbound_message_id}` | runs §8.4 chain, `external_effect=true`; dedupe_key `dispatch:<outbound_message_id>` |
| `sending.reconcile_unknown` | sending | sending | `{outbound_message_id}` | §8.5 lookup only, never resubmits |
| `campaigns.freeze_launch` | campaigns | campaigns (POST launches) | `{campaign_launch_id}` | A.6.2 build → frozen → approval request |
| `campaigns.activate_launch` | campaigns | approvals (launch approved) | `{campaign_launch_id, approval_request_id}` | schedules step-1 `sending.dispatch_outbound` jobs per time rules |
| `campaigns.schedule_next_step` | campaigns | campaigns (consumer of `outbound.accepted`) | `{campaign_enrollment_id}` | next step relative to previous accepted (DDL 25) |
| `compliance.pause_enrollments` | campaigns | foundation `compliance_core::insert_suppression` | `{email_address_id, suppression_id}` | pause all active enrollments of the address (already enqueued by foundation) |
| `campaigns.pause_address` | campaigns | inbox (human reply / unsubscribe_request / auto_reply), booking (booking requested) | `{email_address_id, reason}` | reason ∈ `human_reply`,`auto_reply_review`,`meeting_requested`,`unsubscribe_request` |
| `booking.confirm` | booking | booking (POST public booking) | `{booking_id}` | calendar create saga §10.3, `external_effect=true` |
| `booking.apply_change` | booking | approvals (booking change approved), booking (host direct reschedule/cancel) | `{booking_change_id}` | reschedule/cancel saga A.6.6 |
| `booking.expire_holds` | booking | booking (periodic self-reschedule) | `{}` | held → expired only (T22) |
| `compliance.release_suppression` | compliance | approvals (release approved) | `{approval_request_id, suppression_id}` | uses `compliance_core::release_suppression` |
| `inbox.sync_mailbox` | inbox | inbox (periodic), webhooks ingress | `{mail_account_id, reason}` | overlap re-scan + upsert §9.1 |
| `inbox.process_webhook` | inbox | webhooks ingress | `{webhook_event_id}` | |
| `imports.validate` / `imports.commit` | imports | imports | `{import_job_id}` | |
| `imports.export` | imports | imports | `{export_job_id}` (row in `import_jobs` kind=export) | |
| `crm.merge_companies` | crm | crm | `{merge_request: {source_id, target_id, field_choices}, requested_by}` | |
| `org.disable_principal` | org | org | `{principal_id, reassign_to, cancel_pending}` | A.7 |
| `org.revoke_credential_jobs` | org | org (credential revoke) | `{credential_id}` | cancels not-yet-dispatched outbound/agent work §5.5 |
| `outbox.dispatch` | operations (framework) | worker loop | — | built in foundation |

## Outbox events (`event_type` → producer → payload → consumers)

| event_type | producer | payload | consumers |
|---|---|---|---|
| `lead.accepted` | research | `{lead_proposal_id, company_id, contact_id?}` | crm activity projector |
| `campaign.frozen` | campaigns | `{campaign_launch_id, manifest_hash_hex, approval_request_id}` | operations (dashboard) |
| `approval.decided` | approvals | `{approval_request_id, target_type, target_id, decision}` | crm projector, operations |
| `outbound.accepted` | sending | `{outbound_message_id, campaign_enrollment_id?, email_address_id, thread_id?}` | campaigns (schedule next step), crm projector |
| `outbound.unknown` | sending | `{outbound_message_id}` | operations (alert) |
| `outbound.failed` | sending | `{outbound_message_id, permanent}` | campaigns |
| `message.received` | inbox | `{email_message_id, thread_id, mail_account_id, from_email_address_id?, classification}` | campaigns (pause per §8.4 rules), crm projector |
| `suppression.created` | foundation compliance_core | `{email_address_id, purpose_scope, reason}` | crm projector |
| `booking.requested` | booking | `{booking_id, email_address_id?}` | campaigns (pause follow-ups) |
| `booking.confirmed` | booking | `{booking_id, company_id?, contact_id?}` | crm projector |
| `principal.disabled` | org | `{principal_id}` | operations |

Producers emit; consumers subscribe via `EventConsumer` (see foundation). A consumer runs in the same transaction that inserts its
`processed_events` row, so a crash re-delivers and dedupe makes it effectively-once for DB effects.

## Approval request creation (shared table `approval_requests`)

Requesting modules insert their own rows (DDL 49: exactly one concrete target FK) through **`infrastructure::repo::approvals::insert_request`** (owned by the
approvals module but its signature is fixed here so others can call it after merge; in Wave B, other modules call it and approvals implements it — the
foundation fix round adds this function as a working implementation):

```rust
// maps to action_type campaign_launch | message_submit | booking_change | suppression_release (DDL line 1964)
pub enum ApprovalTarget { CampaignLaunch(Uuid), OutboundMessage(Uuid), BookingChange(Uuid), SuppressionRelease(Uuid) }
pub async fn insert_request(tx: &mut Tx<'_>, requested_by: Uuid, target: ApprovalTarget,
    snapshot_hash: &[u8; 32], expires_at: DateTime<Utc>) -> Result<Uuid, AppError>;
```
The requester's free-text reason (B.3.8) goes into the audit entry metadata, not the table (no column).
Decision side effects (approvals module): launch → enqueue `campaigns.activate_launch`; outbound → set `queued` + enqueue `sending.dispatch_outbound`;
booking change → enqueue `booking.apply_change`; suppression release → enqueue `compliance.release_suppression`. All in the decision transaction (A.6.3).

## Activities projection

`activities` rows are written **only** by the crm projector consumer (plus crm's own notes/tasks). Each row uses the outbox event id as `source_event_id`
(UNIQUE with `kind`, so re-delivery is idempotent), the domain ids (`thread_id`, `booking_id`, …) and, for mail-derived rows, `mail_account_id`
so reads filter by mailbox grant (§5.6). `summary` holds only non-sensitive labels (no subject/body for mail rows; the read path joins and
re-checks the grant before revealing subject).
