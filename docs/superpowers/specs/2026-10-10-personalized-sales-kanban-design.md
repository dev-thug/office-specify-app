# Personalized sales campaign board

The user wants one campaign to organize outreach to multiple companies, with different researched copy per company. SMTP/reply configuration belongs in Settings, not a repeated campaign wizard. A kanban board should connect research, drafts, exact-message human review, a ten-minute sending batch, and actual provider results.

## Behavior

- Primary campaigns are personalized sales groups; preserve existing common-template campaigns under an explicitly labeled bulk path. Never migrate or reinterpret old approved snapshots.
- Create a group with name/objective and optional meeting type. Resolve its sender from an explicit workspace default active marketing SMTP profile, or the sole eligible active profile. Bind mailbox/channel/sender at creation. Show a readable sender summary, not mailbox/channel selectors or QA IDs.
- A configured group meeting type is pinned. If none was configured, the existing per-message meeting-type choice at freeze is permitted with normal host access checks; adding a meeting button later must not require recreating the campaign.
- Admins set the default sender in Mail accounts. Missing, disabled, ambiguous or unauthorized defaults require a Settings link; do not silently choose another account. Changing the default does not change existing groups.
- Each company/contact/email target is verified against CRM ownership in the same workspace. One group can hold many targets. Research/evidence remain in existing CRM. A target may have multiple explicit message drafts over time; its current draft pointer is authoritative.
- A group holds at most 200 targets, enforced under a campaign-row lock when adding. The board returns the full bounded set; it must never silently truncate a group.
- Columns: researched, draft, review, queued, accepted, attention. Only pre-send workflow stages are editable. Queued/accepted/failed/unknown derive from current outbound/approval records, never a client-supplied status.
- Card detail shows company research, sources, editable subject/body and reusable template, rendered recipient preview, and existing freeze/approval/send status. Draft creation shares content to authorized mailbox reviewers, captures original service credential, and uses existing immutable sending APIs.
- Moving a card to queued requires a current independently approved frozen message. The server schedules `not_before` to the next UTC epoch multiple of 600 seconds. Existing durable dispatch jobs execute the batch, rechecking grants, source credential, approval/hash, suppression, quiet hours and caps. Moving is not approval. Unknown sends are not automatically retried.
- Generic inbox freeze/submit and `mail_submit_approved` cannot bypass group rules: freeze validates the pinned sender/recipient/current draft, and submission aligns linked messages to a ten-minute boundary. Additive draft linkage IDs let the generic UI route authors back to the board.
- Poll the board every two seconds. Accepted means SMTP provider accepted, not proof of inbox delivery. Failed/unknown work appears in attention. Include keyboard/button alternatives to drag/drop.
- Agent tools can create/read groups, add verified targets, create different target drafts, move pre-send stages, and queue only approved messages using `mail.submit_approved`; no agent approval or direct-send scope.

## Interfaces and ownership

New REST module `/api/v1/sales-campaigns`: GET/POST groups, GET group, POST `{id}/targets`, PATCH `{id}/targets/{target_id}` stage, POST `{id}/targets/{target_id}/drafts`, POST `{id}/targets/{target_id}/queue`. GET/PUT `/sales-campaigns/sender` reads/sets default SMTP profile.

New contracts/application/repository `sales_campaigns` modules and migration 0042. Tables hold groups, targets/current draft, message associations and workspace sender default. Apply forced workspace RLS, composite foreign keys, current mailbox grants and bounded pages; never put message bodies or credentials in audit metadata.

Queue request: `outbound_message_id`, `expected_payload_hash`, `expected_target_version`. Response includes existing submission fields plus `batch_at`. Outer idempotency caches the computed batch instant; identical retries across boundaries must replay the same job/time.

Sending owner exposes `submit_scheduled_approved_in_tx(tx: &mut Tx<'_>, ctx: &Ctx, id: Uuid, expected_payload_hash: &str, not_before: DateTime<Utc>) -> Result<SubmitOutboundResponse, AppError>` using the existing submit policy. Credential calls use approval-bound mode; human calls remain existing human marketing policy. The new group module owns the outer idempotency transaction and target/outbound binding checks.

Backend owner publishes exact DTO/function names before MCP/UI binding. Root owns module/router/OpenAPI/MCP registry registration, generated contracts, docs, local migration/deployment and final verification. No real email is sent for this new work without a new specific test instruction; fake SMTP and canceled local test queue validate scheduling.

## Acceptance

Different companies in one group retain different drafts and correct recipients; stale/foreign target/draft IDs are denied. Approval is never implied by moving stages. Duplicate queue requests and boundary-crossing replays produce one durable job. SMTP acceptance moves the card to accepted; unknown stays attention. Default-sender changes do not retarget existing groups. Human review and MCP source-credential gates remain intact. Local UI verifies editor/preview, stage movement and live status refresh without extra real mail.
