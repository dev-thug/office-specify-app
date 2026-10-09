# Zoho Campaigns Free Bulk Delivery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox syntax for tracking.

**Goal:** Send one approved, immediate Office marketing launch as one Zoho Campaigns v1.1 bulk campaign while keeping Office consent, suppression, and per-recipient booking links authoritative.

**Architecture:** Reuse the existing OAuth connection, campaign launch and human approval chain. Add a durable export snapshot, provider list/contact mapping, and a write-ahead operation journal. Create one provider campaign and call `sendcampaign` once for that launch; never dispatch Campaigns once per recipient. Campaigns’ native opt-out tag is reconciled into Office suppression by polling, while Office suppressions are pushed to Zoho before later sends.

**Tech Stack:** Existing Rust/Axum/SQLx/PostgreSQL workspace, `CampaignsHttp`, SecretStore, durable jobs/outbox, Office campaign and compliance/booking services, Next.js campaign and marketing screens, local HTTP fixtures.

**Spec:** `docs/superpowers/specs/2026-10-09-office-zoho-campaigns-design.md`; capability evidence in `docs/design/zoho-api-capability-matrix.md`; this plan implements the unfinished Free Campaigns delivery/export/report path only.

## Global Constraints

- Use the existing Free Campaigns OAuth v1.1 connection at the fixed US `.com` hosts. Do not use Zoho Mail for marketing or the separate prepaid Email API v2 key.
- Campaigns v1.1 template CRUD was not verified. Copy/import a Campaigns template into the existing Office visual/HTML editor and publish the Office version through `content_url`; do not route this Free plan through the separate Email API v2 template library.
- Allow only one immediate Campaigns step. Do not add schedules, workflows, automated follow-ups, webhooks, or repeated `sendcampaign` calls.
- Keep Office templates as the editing source. Publish one immutable, publicly fetchable HTTPS `content_url` snapshot per export; respect Campaigns’ 300,000-character content ceiling.
- Only recipients with current Office marketing basis and no active marketing/all suppression enter the provider list. Re-check eligibility immediately before one bulk send.
- `listsubscribe` success with confirmation enabled means confirmation was requested, not that the recipient is active. Pending confirmation blocks export readiness.
- Preserve per-recipient `meeting.booking_url` by writing each Office-issued URL to a verified Campaigns contact field and rendering the corresponding Campaigns merge tag. A shared booking URL, recipient loop of campaign sends, or guessed merge syntax is forbidden.
- Campaign HTML uses exactly one Campaigns-native unsubscribe tag. Zoho’s review guidance rejects external unsubscribe links and multiple unsubscribe links; do not include Office’s generic unsubscribe URL in exported Campaigns HTML unless the account owner verifies an allowed native mapping. Office remains the suppression source of truth through bidirectional sync.
- Commit journal intent before each provider write. Unknown create/send outcomes are never automatically repeated; reconcile by provider IDs/status or require human review.
- New tables use workspace-composite keys/FKs, RLS, and minimal API/worker grants. Logs, audits, job payloads, and idempotency records contain IDs, hashes, counts and provider codes only; no tokens, recipient payloads, booking URLs or HTML.
- Test with synthetic data and loopback provider fixtures. A live test-recipient send is a separate, explicit human action after account verification.

## Review Focus

1. Zoho list targeting includes exactly the Office-approved addresses, including when the provider response is paged or contains stale contacts — Tasks 1 and 3.
2. `listsubscribe` returns code `0` but the list requires confirmation — Task 3 must keep the export blocked until the address is verified active.
3. Office or Campaigns opt-out arrives during staging or before submission — Tasks 4 and 5 must stop the bulk send or create a new approval snapshot.
4. A booking URL is assigned to the wrong recipient or reused across addresses — Task 3 must bind every mapped URL to one Office `email_address_id` and test cross-recipient isolation.
5. `createCampaign` or `sendcampaign` times out after Zoho may have accepted it — Tasks 2 and 5 must persist `unknown` and never resubmit automatically.

---

## Phase 0 — Contract and account readiness gate

No delivery code is enabled until these checks are recorded against the intended Free Campaigns organization. Use only a test list and test addresses; do not send a real campaign as part of this phase.

**Current source baseline:** `crates/infrastructure/src/providers/zoho_campaigns/mod.rs` defines `CAMPAIGNS_SCOPES` with only `campaign.READ` and `contact.READ`; `http.rs` currently provides OAuth exchange/refresh, list/recent reads and an allowlisted read-only `get`. `application/src/marketing/mod.rs` and `api/src/routes/marketing.rs` expose setup/connect/list/recent/disconnect only. In contrast, `campaigns/freeze.rs` stores one local outbound per enrollment and `campaigns/flow.rs::activate_launch` enqueues `sending.dispatch_outbound` jobs. The new Zoho branch must stop at the one bulk `marketing.send_campaign` job.

### Verified API surface to implement

| Purpose | Documented v1.1 operation | Key constraint |
|---|---|---|
| Create a campaign draft | `POST /api/v1.1/createCampaign` | Form encoded; requires campaign name, sender, subject, `content_url`, and `list_details`; `topicId` is mandatory for accounts on updated topic management. Content must be publicly fetchable HTML and at most 300,000 characters. [Zoho create campaign](https://www.zoho.com/campaigns/help/developers/create-campaign.html) |
| Submit the campaign | `POST /api/v1.1/sendcampaign` with `campaignkey` | Requires `ZohoCampaigns.campaign.UPDATE`. Response `code=200`, `campaign_status=inprogress` means accepted/in progress, not delivered. [Zoho send campaign](https://www.zoho.com/campaigns/help/developers/send-campaign.html) |
| Create an export-specific list | `POST /api/v1.1/addlistandcontacts` | Requires `mode=newlist`, `listname`, `signupform=private`, and 1–10 `emailids`; requires `contact.CREATE`. Verify private-list confirmation behavior. [Zoho add new list and contacts](https://www.zoho.com/campaigns/help/developers/add-new-list-contact.html) |
| Add more list members | `POST /api/v1.1/addlistsubscribersinbulk` | Maximum 10 email IDs per request; this operation does not set the per-contact booking field. [Zoho add contacts in existing list](https://www.zoho.com/campaigns/help/developers/add-contacts-existing-list.html) |
| Upsert contact fields/topic | `POST /api/v1.1/json/listsubscribe` | `contactinfo` carries contact fields and optional `topic_id`; list signup confirmation can leave the member pending. [Zoho subscribe](https://www.zoho.com/campaigns/help/developers/contact-subscribe.html) |
| Read exact list status | `GET /api/v1.1/getlistsubscribers` with `listkey`, `status`, `fromindex`, `range` | Documented statuses include `active`, `unsub`, and `bounce`; this is not an organization-wide Do-Not-Mail registry read. [Zoho list contacts](https://www.zoho.com/campaigns/help/developers/get-list-subscribers.html) |
| Add a provider suppression | `POST /api/v1.1/json/listunsubscribe`; `POST /api/v1.1/json/contactdonotmail` | The former is list-scoped; the latter adds to the account-wide Do-Not-Mail registry. No full registry read API was verified. [Zoho unsubscribe](https://www.zoho.com/campaigns/help/developers/contact-unsubscribe.html), [Zoho Do-Not-Mail](https://www.zoho.com/campaigns/help/developers/do-not-mail.html) |
| Read Campaigns opt-outs/events | `POST /api/v1.1/getcampaignrecipientsdata` with `campaignkey` and an `action`, including `optoutcontacts`, `spamcontacts` and bounce actions | The reference’s request parameters do not document `fromindex`/`range`, although its response example returns them. Do not invent pagination or claim full coverage until the target account proves the paging contract. [Zoho recipient data](https://www.zoho.com/campaigns/help/developers/campaign-recipient-data.html) |
| Read aggregate reports | `GET/POST /api/v1.1/campaignreports` with `campaignkey` | Counts are aggregate evidence only; they do not establish which specific recipient was delivered/opened. [Zoho campaign reports](https://www.zoho.com/campaigns/help/developers/campaign-reports.html) |

### Account checks that gate delivery

- Confirm the Free plan, immediate-send entitlement, verified sender, list/topic model, Campaigns-native unsubscribe rendering, and the account’s current rate/recipient limits. Free does not provide scheduled campaigns, workflows or webhooks. [Plan comparison](https://www.zoho.com/campaigns/comparison.html)
- Verify that a newly created private list, populated only from the frozen Office audience, is the exact audience of `createCampaign`. If `list_details` selects anything beyond that list, or Zoho includes stale members, do not enable send.
- In the account’s template preview, prove that Office `meeting.booking_url` can map to one verified contact custom field and Campaigns merge tag. The docs around custom field/merge-tag creation are inconsistent; create the field/tag in the Zoho UI for the test account if needed. If the field value cannot be read back for the matching contact, booking-enabled campaigns remain blocked.
- Use exactly one `$[LI:ORG_OPTOUT]$` marker in the exported Campaigns HTML so the native action is account-wide for this Campaigns-only marketing organization. Verify the account actually treats it as Do-Not-Mail and that optout/report/list reads provide an Office mapping; otherwise stop the Free Campaigns send path.
- Prove which Campaigns-native opt-out action is generated by the selected single tag and how it appears in `status=unsub` and `optoutcontacts`. The Free plan has no webhook and the reviewed API has no full Do-Not-Mail list read. If every provider-native opt-out cannot be reliably mapped back to Office before the next send, leave bulk send disabled.
- The native tag and Office link are not interchangeable. If the requirement is that a click visibly lands on Office’s `/public/v1/unsubscribe` page, stop here unless Zoho verifies that native Campaigns unsubscribe can safely route to that page. Do not work around Zoho’s external-link/review rule by inserting both links.
- Verify `getcampaignrecipientsdata` coverage/paging and `getcampaigndetails` fields in the real account. Without documented or account-verified paging, report coverage stays partial and that endpoint cannot be the sole unsubscribe source.

**Phase 0 deliverable:** an account contract record in `docs/runbooks/zoho-campaigns.md` with the verified list, sender, topic, field/merge tag, native opt-out behavior, report coverage, and current API limits. Missing evidence is a blocker; do not fill it with assumed provider behavior.

---

## File ownership and boundaries

| Owner | Files and responsibility |
|---|---|
| Schema/repository | Create `migrations/0022_zoho_campaigns_delivery.sql`; create `crates/infrastructure/src/repo/marketing_exports.rs`; register it in `crates/infrastructure/src/repo/mod.rs`. Keep OAuth-only connection code in the existing `repo/marketing.rs`. |
| Contracts/domain | Extend `crates/contracts/src/marketing.rs`; create `crates/domain/src/marketing.rs` for export/send states, state transitions, and provider capability validation; register in the crate roots. |
| Zoho provider | Extend `crates/infrastructure/src/providers/zoho_campaigns/http.rs` only for shared bounded transport and OAuth token use; create `contacts.rs`, `campaigns.rs`, `reports.rs`, and `crates/infrastructure/tests/zoho_campaigns_delivery.rs`. Add only typed fixed-path operations; keep secrets in `SecretValue`. |
| Export/application | Create `crates/application/src/marketing/exports.rs`, `audience.rs`, `content.rs`, `dispatch.rs`, `suppressions.rs`, `reports.rs`; `exports.rs` owns the Zoho-only branch in `campaigns/freeze.rs` and final export approval-request creation. |
| Approval/activation | `approvals/mod.rs`, `infrastructure/src/repo/approvals.rs`, `campaigns/flow.rs`, and `worker/src/handlers/campaigns.rs` own only decision re-verification and the mutually exclusive bulk activation branch; they do not edit `campaigns/freeze.rs`. |
| Booking contract | Modify `crates/application/src/booking/mod.rs` only to expose a typed Office booking-link identity with the existing URL, and `crates/infrastructure/src/repo/booking.rs` only if the export needs a workspace-bound lookup. Keep token creation, binding, and resolution in Office. |
| API/worker | Extend `crates/api/src/routes/marketing.rs`; add `crates/api/tests/marketing_campaigns.rs`; create `crates/worker/src/handlers/marketing.rs` and `crates/worker/tests/marketing_export.rs`, `marketing_dispatch.rs`, `marketing_reports.rs`; register the handler in `crates/worker/src/lib.rs`. |
| UI/operations | Extend `apps/web/src/features/marketing/` and `apps/web/src/features/campaigns/campaign-builder.tsx`, their tests and locales; update `apps/web/src/lib/api/marketing.ts` after OpenAPI generation; create `docs/runbooks/zoho-campaigns.md`. |

`migrations/0018_zoho_campaigns.sql` through `0021_zoho_template_operations.sql` are existing work: do not edit or reuse them. Email API v2 routes and `zoho_mail` dispatch remain separate. The same launch must never reach both `campaigns.activate_launch` and `marketing.send_campaign`.

The application export/worker owner holds `application/src/marketing/exports.rs` and `worker/src/handlers/marketing.rs` across Tasks 3–5. Those tasks run sequentially and must not edit those files in parallel; provider adapter work in Task 2 is the independent branch.

## Dependency order

```text
Phase 0 account contract gate
        ↓
Task 1 durable export/schema + Task 2 typed provider API (either order after the contract)
        ↓
Task 3 prepare export, contact list, merge fields and immutable content
        ↓
Task 4 native opt-out mapping, Office suppression propagation, report poll/checkpoints
        ↓
Task 5 approval-bound single bulk send and lookup-only recovery
        ↓
Task 6 UI, runbook, synthetic end-to-end tests and final enablement gate
```

Tasks 1 and 2 may be implemented independently after Phase 0, but Task 3 consumes both. Task 5 is not enabled until Task 4’s suppression freshness and opt-out coverage tests pass. Task 6 is the only phase that considers a real test-recipient send, and that send requires separate explicit human confirmation.

## Task 1 — Durable export, recipient map, and operation journal

**Files:** Create `migrations/0022_zoho_campaigns_delivery.sql`, `crates/domain/src/marketing.rs`, `crates/infrastructure/src/repo/marketing_exports.rs`, `crates/infrastructure/tests/marketing_exports.rs`. Extend the existing `crates/contracts/src/marketing.rs`; register the new modules.

**Interfaces:**

- `MarketingExportState`: `preparing`, `contact_sync`, `draft_ready`, `needs_review`, `approved`, `submitting`, `in_progress`, `completed`, `rejected`, `unknown`, `canceled`.
- `MarketingRecipientState`: `pending`, `syncing`, `active`, `pending_confirmation`, `unsubscribed`, `bounced`, `excluded`, `unknown`.
- `Claim`: `New`, `AlreadyCompleted { provider_id }`, or `Unknown`; `Unknown` can only be reconciled by a provider read.
- `ExportReadiness { export_id, state, total, active, pending_confirmation, excluded, approval_hash }`; `SuppressionSyncSummary { queued, synced, unknown, blocked }`; `ReportSyncSummary { observed_at, coverage_status, aggregate_totals, recipient_events_seen }` expose counts/status only, never contact fields or tokens.
- `DispatchOutcome`: `Accepted { status }`, `Rejected { code }`, or `Unknown`; `ReconcileOutcome`: `Accepted { status }` or `StillUnknown`.
- `snapshot_export_in_tx(tx, ctx, launch_id) -> MarketingExportSummary` binds one local snapshot to `(workspace_id, launch_id, step_id)` and makes no provider call.
- `claim_operation(tx, export_id, kind, correlation_key, request_hmac)`, `complete_operation(...)`, `mark_operation_unknown(...)` and read-only reconciliation helpers store provider IDs/status only.
- Worker orchestration uses short `Db` transactions around `CampaignsHttp` calls; no SQL transaction is held while waiting on Zoho.

- [ ] Write DB tests first for unique one-export-per-launch-step, duplicate recipient rejection, workspace-composite FK, RLS isolation, and worker/API versus report-reader grants.
- [ ] Write domain tests that reject multi-step, delayed, or scheduled launches for `zoho_campaigns`; existing launch validation remains source of truth for other providers.
- [ ] Add `marketing_exports` with workspace, launch, revision, step, source manifest hash, audience/content/field-map hashes, provider connection/list/topic/sender IDs, provider campaign key, state, approval hash, and last suppression/status check. Add `marketing_export_recipients` with `email_address_id`, source frozen outbound ID, optional provider contact ID, Office booking-link ID, sync/confirmation state and field-map hash. Add `marketing_report_checkpoints` keyed by export/action and `marketing_recipient_events` keyed by export/action/provider contact/event key. Do not store raw email, HTML, booking URLs, OAuth tokens, or `contactinfo` request bodies in journal rows.
- [ ] Add `marketing_operation_journal` with unique `(workspace_id, export_id, kind, correlation_key)` and only HMAC request fingerprints, intent/unknown/completed state, provider object IDs, and safe provider codes. Add `marketing_suppression_sync` keyed by the Office `suppression_id` and connection so Office suppression propagation is durable independently of an export.
- [ ] Add workspace-composite foreign keys and RLS to all tables. Grant only the DML required to `api_runtime` and `worker_runtime`; deny `report_reader`. Verify migration on a fresh dedicated synthetic database and commit the schema/contracts/repository slice.

The red tests must prove the non-duplication invariant before implementation: two inserts with the same `(workspace, launch, step)` produce one export; an `unknown` `send_campaign` journal row cannot be claimed for another provider write.

```rust
assert!(matches!(repo::claim_operation(&mut tx, export_id, "send_campaign", key, hash).await?, Claim::New));
repo::mark_operation_unknown(&mut tx, export_id, "send_campaign", key).await?;
assert!(matches!(repo::claim_operation(&mut tx, export_id, "send_campaign", key, hash).await?, Claim::Unknown));
```

## Task 2 — Typed Campaigns v1.1 write/read adapter

**Files:** Extend `crates/infrastructure/src/providers/zoho_campaigns/http.rs`; create `contacts.rs`, `campaigns.rs`, `reports.rs`, and `crates/infrastructure/tests/zoho_campaigns_delivery.rs`; update `crates/infrastructure/src/providers/zoho_campaigns/mod.rs` only for module registration/scopes.

**Produces:** Typed `CampaignsHttp` methods for `addlistandcontacts`, `addlistsubscribersinbulk`, `listsubscribe`, `getlistsubscribers`, `listunsubscribe`, `contactdonotmail`, `createCampaign`, `getcampaigndetails`, `sendcampaign`, `campaignreports`, and `getcampaignrecipientsdata`. No public arbitrary path/body method.

Mutation results use a closed type, for example `SendResult::{Accepted { campaign_key, status }, Rejected { code }, Unknown}`; they never return raw provider bodies.

`CampaignsHttp` remains the owner of the fixed host, token header, client and bounded response reader in `http.rs`. If contacts/campaigns/reports logic is split into sibling modules, expose only `pub(super)` request helpers taking a closed `CampaignsEndpoint` enum; do not expose arbitrary URLs or user-supplied paths.

- [ ] For each method, assert its exact fixed v1.1 path, verb, JSON/form encoding and response shape against loopback fixtures. Preserve the current fixed `.com` host, OAuth header, redirect denial, size bounds and timeout.
- [ ] Parse provider business codes as strings or numbers and recognize endpoint-specific success: list/contact operations documented as code `0`; `createCampaign` code `200` plus `campaignKey`; `sendcampaign` nested `response.code=200` plus `campaign_status` such as `inprogress`. HTTP 200 by itself is never success.
- [ ] Enforce the documented ten-email maximum for `addlistandcontacts` and `addlistsubscribersinbulk`. `listsubscribe` test must distinguish a confirmation-requested response from an active subscription.
- [ ] Add only the granular OAuth scopes required by these methods: current `CAMPAIGNS_SCOPES` has `campaign.READ` and `contact.READ`; delivery adds `campaign.CREATE`, `campaign.UPDATE`, `contact.CREATE`, and `contact.UPDATE`. Do not request `ALL` or DELETE. Existing connections missing newly required scopes must enter reconnect/reauth-required and cannot send.
- [ ] Add mock outcomes for 401/reauth, 429/Retry-After, 5xx, timeout, redirect, malformed JSON, code-level rejection and ambiguous successful response. Create/list/send writes map uncertainty to `Unknown`. The adapter never retries a write internally.
- [ ] Until Phase 0 confirms recipient report paging and contact fields in the target account, the adapter accepts only documented report parameters. Specifically, do not add guessed `fromindex`/`range` to `getcampaignrecipientsdata` or guessed merge-field APIs.

One mock-contract test must show that an HTTP 200 business rejection is not accepted as a campaign key or send acceptance:

```rust
assert!(matches!(parse_campaign_send(json!({"response":{"code":"6101"}})), SendResult::Rejected { .. }));
assert!(matches!(parse_campaign_send(json!({"response":{"code":"200","campaign_status":"inprogress"}})), SendResult::Accepted { .. }));
```

## Task 3 — Export, exact audience, Office content, and booking merge field

**Files:** Create `crates/application/src/marketing/{exports,audience,content}.rs`, `crates/worker/src/handlers/marketing.rs`, and `crates/worker/tests/marketing_export.rs`; modify `campaigns/preflight.rs` and `campaigns/freeze.rs`; extend `booking::issue_bound_link_in_tx` with a typed `BookingLinkIssue { id, url }` result without changing the public booking URL behavior; register the new worker handler. This task exclusively owns `campaigns/freeze.rs` changes.

**Interfaces:** `prepare_export(db, export_id) -> Result<ExportReadiness, AppError>` is worker orchestration; `finalize_export_in_tx(tx, export_id) -> Result<Uuid /* approval request id */, AppError>` runs only after provider checks. `prepare_export` owns the bounded calls and does not hold a SQL transaction across network I/O.

- [ ] Freeze one local export snapshot from the existing single-step launch: exact deduplicated `email_address_id` set, current marketing-basis/suppression result, selected connection/sender/topic, Office template version, transformed HTML hash, merge-field map version, and a stable list-creation correlation. The provider list key and campaign key are filled only after provider preparation and added to the final approval hash. The launch’s existing per-recipient frozen outbound remains a local snapshot source only; it must not become a provider send loop.
- [ ] In the Zoho-only `campaigns.freeze_launch` branch, commit the local frozen manifest/export rows in `preparing`, leave the launch in `frozen`, and enqueue `marketing.prepare_export`. Do not create an approval request yet. Non-Zoho freeze/approval behavior is unchanged.
- [ ] Separate local snapshot work from provider I/O: `snapshot_export_in_tx` stores local IDs/hashes and queued job IDs; `prepare_export(db, export_id)` commits intent, calls Zoho outside the SQL transaction, and persists the result in a second short transaction.
- [ ] Create a fresh private Campaigns list for each export. Use `addlistandcontacts` for the first 1–10 selected contacts and `addlistsubscribersinbulk` for later batches of at most ten only after the account gate proves this list behavior. Give the list a stable non-PII name containing the export correlation ID. If list creation outcome is unknown, find the exact unique list by that stable name before continuing; do not create a second list blindly.
- [ ] Set per-contact custom values with `listsubscribe`: exact normalized address, topic, and the Office `meeting.booking_url` value associated with that recipient’s `email_address_id`. Read the list back with `getlistsubscribers(status=active)` and compare provider members/fields against the frozen export. Any extra, missing, pending-confirmation, bounced or unsubscribed address keeps the export out of `draft_ready`.
- [ ] Build `content_url` from the Office template version, not from any one recipient’s rendered outbound. Compile supported Office fields to the account-verified Campaigns merge tags. Compile `meeting.booking_url` to the verified booking field tag. Replace the Office unsubscribe variable/footer with exactly one Campaigns-native tag. Preserve the Office source template version unchanged; preview the transformed Campaigns HTML in a sandbox.
- [ ] Preserve booking recovery without duplicating plaintext in job/audit rows: map each export recipient to the Office `booking_link_id` and frozen source outbound ID; expose a checked internal helper that confirms the source link is bound to the same `email_address_id` before its URL is sent as a contact field. Store the recoverable URL only in the existing encrypted `SecretStore` under an opaque workspace/export/recipient reference; the DB stores that reference, not the URL. Retain it through an unknown contact-sync result, then delete it after provider readback confirms the exact field value or the export is canceled; cancellation also revokes the unused Office booking link. If an unknown contact-sync request cannot be reconciled by a readback, keep it `unknown`; never mint another live link or send until an operator resolves it.
- [ ] Verify transformed HTML remains under Zoho’s 300,000-character limit and `content_url` is public HTTPS, immutable, bounded, `no-store`/`noindex`, workspace/export-bound and log-redacted. A missing public origin blocks provider draft creation, not just send.
- [ ] When list, contact fields, campaign draft, Zoho review state and opt-out/booking checks are all verified, finalize the provider-bound approval hash in a short transaction, transition the frozen launch to `awaiting_approval`, and insert `ApprovalTarget::CampaignLaunch` with that hash. This is the only Zoho path that opens Office approval.
- [ ] Provide `GET /api/v1/marketing/exports/{id}`, `GET /api/v1/marketing/exports/{id}/reports`, and idempotent refresh enqueue `POST /api/v1/marketing/exports/{id}/refresh`. Enforce the existing campaign/mailbox read scope for reads and human session plus the existing campaign compose scope for refresh. The export is prepared by the worker from the frozen launch; these routes cannot create or send a campaign directly.

The export test compares the full sorted Office address-ID set to the active provider membership and the `meeting.booking_url` field hash per recipient; a mismatch must keep the export out of `draft_ready`.

## Task 4 — Native opt-out mapping, Office suppression push, and reports

**Files:** Create `crates/application/src/marketing/{suppressions,reports}.rs`; extend `crates/infrastructure/src/repo/marketing_exports.rs`, `crates/worker/src/handlers/marketing.rs`, worker outbox/job registration, `crates/api/src/routes/marketing.rs`; add `crates/worker/tests/marketing_reports.rs`.

**Interfaces:** `propagate_suppression(db, suppression_id) -> Result<SuppressionSyncSummary, AppError>`, `poll_marketing_reports(db, export_id) -> Result<ReportSyncSummary, AppError>`, and `ingest_native_optout(tx, export_id, provider_contact_id) -> Result<Uuid /* Office suppression id */, AppError>`.

- [ ] Keep Office `compliance_core::insert_suppression` as the canonical write. Consume its committed suppression/outbox event asynchronously. For active marketing/all suppressions, persist an operation and use `contactdonotmail`; also use `listunsubscribe` for every still-linked export list. Provider `contactdonotmail` code `0` includes “already in Do-Not-Mail” and is treated as the terminal state. Never auto-remove from provider Do-Not-Mail after an Office release; its removal behavior/read path is unverified and a release remains a reviewed Office action.
- [ ] Poll `getlistsubscribers(status=unsub)` for every active export list using documented `fromindex`/`range`. Poll campaign `optoutcontacts`, `spamcontacts`, and bounce actions only with the verified request/paging contract. Deduplicate repeated/overlapping rows by a durable provider event/contact/action key; update Office suppression in a workspace transaction via `compliance_core::insert_suppression`, mapping contacts through `marketing_export_recipients` to the original `email_address_id`.
- [ ] Map an opt-out made through `$[LI:ORG_OPTOUT]$` to Office `purpose_scope='marketing'`, `reason='unsubscribe'`, `source_kind='zoho_campaigns_sync'`; map reported complaints and hard bounces to their corresponding Office reasons. Never import an unrecognized email into CRM or clear an Office suppression because a contact appears active in a later provider read.
- [ ] Store `campaignreports` aggregates separately from recipient events, along with `observed_at` and `coverage_status`. Do not mark every recipient delivered/opened from aggregate counts. Because recipient-report paging is not specified in the public request table, mark event coverage partial until Phase 0 proves all pages are retrievable.
- [ ] Add a maximum suppression freshness threshold. Before `marketing.send_campaign`, require all queued Office suppressions to be propagated and provider opt-out polling to be within that threshold; otherwise block and enqueue refresh. An unsubscribe or complaint arriving before submission invalidates the audience hash and requires a new export/approval.
- [ ] Add tests for duplicate poll pages, unknown recipient mapping, repeated native opt-out, unsubscribe during export preparation, partial DNM propagation, report endpoint with no rows, and blocked send on stale/unknown suppression coverage.

The compliance test must exercise the existing Office primitive, not create a second suppression system:

```rust
ingest_native_optout(&mut tx, export_id, provider_contact_id).await?;
assert!(compliance_core::active_suppressions(tx, email_address_id, "marketing").await?.iter()
    .any(|s| s.reason == "unsubscribe"));
```

## Task 5 — Approval-bound single Campaigns send and unknown recovery

**Files:** Modify `crates/application/src/campaigns/flow.rs`, `crates/application/src/approvals/mod.rs`, `crates/infrastructure/src/repo/approvals.rs`, `crates/worker/src/handlers/campaigns.rs`; create/extend `crates/application/src/marketing/dispatch.rs`, `crates/worker/src/handlers/marketing.rs`, `crates/worker/tests/marketing_dispatch.rs`, `crates/api/tests/marketing_campaigns.rs`. This task does not edit `campaigns/freeze.rs`.

**Interfaces:** `dispatch_campaign(db, export_id, approval_request_id) -> Result<DispatchOutcome, AppError>` performs one bulk write only; `reconcile_campaign(db, export_id) -> Result<ReconcileOutcome, AppError>` performs provider reads only.

- [ ] Extend approval re-verification for `zoho_campaigns` so it checks the export’s composed hash and `draft_ready` state. Continue requiring the existing human admin/manager approver and current permission/mailbox authorization. Agents, service credentials, and MCP tools never call provider send.
- [ ] In `repo::approvals::launch_state`/`approvals::decide`, compare the request snapshot to `marketing_exports.approval_hash` for Zoho launches and to the existing `campaign_launches.manifest_hash` for other providers. Preserve the existing DB decision trigger’s check that the decision hash equals its immutable `approval_requests.snapshot_hash`.
- [ ] On approval, enqueue exactly one deduped `marketing.send_campaign` job for the export. Do not enqueue `campaigns.activate_launch` for this provider. Add defensive guards in `campaigns.activate_launch` and `sending::dispatch` that reject a `zoho_campaigns` channel; test that a Zoho launch has zero `sending.dispatch_outbound` jobs.
- [ ] In the worker, re-lock the launch/export/approval; verify approval still active, not expired/revoked, source/export hashes match, emergency stop is off, sender/connection/topic are active, provider campaign remains eligible, every target still has valid basis and no active suppression, and suppression/report coverage is fresh. A changed membership, body, field map, booking binding or provider review state moves to `needs_review`; never trim recipients and reuse the old approval.
- [ ] Commit a `send_campaign` intent before the only `POST /sendcampaign` call. On documented `code=200` and `campaign_status=inprogress`, persist `accepted/in_progress`; do not label it completed. On definite provider rejection, persist safe rejection. On timeout/reset/5xx/malformed or ambiguous response, persist `unknown` and enqueue only read-only `getcampaigndetails`/`recentcampaigns` reconciliation.
- [ ] Unknown `createCampaign` resolves only by one exact stable provider campaign name/correlation match; unknown `sendcampaign` resolves only by read-only provider campaign status. If the provider data cannot prove what happened, retain `unknown` with an operator action. Never recreate or resend from an unknown row automatically.
- [ ] Test same-key and different-key concurrency, lost worker reply after provider acceptance, approval revocation, changed list/body after approval, suppressed address at final gate, emergency stop, no duplicate direct-send jobs, and unknown never issuing another `sendcampaign` request.

The dispatch fixture must assert one provider-wide submission and zero per-recipient provider sends:

```rust
assert_eq!(mock.count("/api/v1.1/sendcampaign"), 1);
assert!(queued_jobs.iter().all(|job| job.kind != "sending.dispatch_outbound"));
```

## Task 6 — UI, runbook, and final enablement

**Files:** Extend `apps/web/src/features/marketing/`, `apps/web/src/features/campaigns/campaign-builder.tsx`, `apps/web/src/lib/api/marketing.ts`, locales and focused UI/E2E tests; create `docs/runbooks/zoho-campaigns.md`; regenerate OpenAPI/client schemas using existing repo commands.

- [ ] Show only the actual Free-plan path: one step, immediate send, no schedule/workflow controls. Explain that Zoho still controls Campaigns review and native opt-out; a Zoho native opt-out is mirrored into Office on polling, not via webhook.
- [ ] Before approval, show the transformed Campaigns content in a sandbox, exact provider list/audience counts, excluded/pending counts, topic/sender, booking-link mapping, native unsubscribe marker, last suppression check and report coverage. Display the raw Office booking URL only in the per-recipient authorized preview, never in logs, audit, provider job payload or general list views.
- [ ] Show separate states for Office draft, Zoho review, Office approval, send accepted/in progress, completed, rejected, unknown and report staleness. `unknown` offers refresh/reconcile and blocks another send; do not show a retry button that issues a fresh Campaigns send.
- [ ] Add a final send confirmation that states the exact audience count and Campaigns account/list/topic. Deletion/cancellation of a provider campaign is not promised unless documented and verified.
- [ ] Test with local HTTP fixtures and synthetic workspaces: only one `sendcampaign` request; the exact Office template fields and booking values; suppression before/after snapshot; no raw OAuth token/content/booking capability in persisted journal/audit; agent cannot prepare/approve/send; unrelated Zoho Email API v2 and Mail routes remain separate.
- [ ] Add runbook steps for required write-scope reconnection, private export list, verified sender/topic, public HTTPS content origin, native opt-out and booking merge mapping, Free-plan quotas, unknown reconciliation and polling lag.
- [ ] Keep the live Zoho test campaign disabled until all Phase 0 account gates pass. Any final test send uses an explicitly selected test list and human confirmation, after which record mock-versus-live coverage separately.

## Completion checklist

- [ ] One frozen Office launch maps to one private Campaigns list, one Campaigns draft, and at most one `sendcampaign` invocation.
- [ ] No `sending.dispatch_outbound` is queued for a `zoho_campaigns` launch.
- [ ] Every sent address is in the final verified list, has current Office consent, has no Office/provider opt-out, and receives only its own Office booking URL merge value.
- [ ] The exported HTML uses a single verified Campaigns-native unsubscribe tag. If Office’s visible URL is required and Zoho-native mapping is not verified, mark the integration blocked rather than violating review rules.
- [ ] A Campaigns native opt-out creates the matching Office suppression before the next launch can pass its freshness gate; unknown/paged-incomplete provider coverage remains blocked.
- [ ] One confirmed `sendcampaign` response is reported as accepted/in progress; ambiguous response stays unknown and reconciliation never resubmits.
- [ ] API/worker DB-role RLS and audit secrecy tests pass; provider tests use only loopback fixtures; actual account readiness and live test-send evidence are reported separately.
