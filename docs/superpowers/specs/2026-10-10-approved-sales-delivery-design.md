# Approved sales delivery and engagement

User approved separating human approval from execution on 2026-10-10 and explicitly requested implementation and a live test to the operator-approved test inbox.

## Outcome
An agent researches a company, saves cited evidence, prepares an immutable marketing email, asks a human to approve it, executes only the approved snapshot through MCP, and reads submission, replies and engagement. Human approvals remain unavailable to agents. Office owns templates, unsubscribe and bookings; SMTP carries the message.

## Approval and execution
A message_submit approval decision records approval without queueing dispatch. Existing queued/accepted messages are never replayed. Human REST and a new narrowly scoped MCP approved-submit tool use the same application service: current snapshot hash, active unexpired approval, independent human approver, mailbox grants, credential validity, suppression/basis and policy are checked. Approval alone is not send authorization after any bound value changes. An idempotency key fences queue creation, and status distinguishes queued/accepted/unknown/failed from delivery. Agents cannot gain mail.send or mail.approve.

## Reply attribution
Create a mailbox-scoped local thread for new outbound mail and persist the stable SMTP Message-ID. On ingestion, match In-Reply-To and References to this mailbox's outbound correlation; verify the sender is the outbound recipient. Never match solely by subject or across workspaces/mailboxes. Preserve reply chain identifiers in the wire message. Expose linked replies through accessible outbound status and the existing thread reader.

## Engagement
At immutable freeze, optionally issue recipient/outbound-bound opaque tracking capabilities. Rewrite eligible HTTP(S) content links to an Office redirect with its target stored server-side; never accept a caller-supplied redirect destination. Keep unsubscribe and booking URLs direct. Add a transparent pixel. Public endpoints record bounded open-request/click-request events and return a no-cache pixel or redirect. Tokens are not authentication credentials; record no raw IP/user-agent and no token values in audit logs. Disable tracking unless the draft freeze explicitly opts in, or when a workspace disables it; explain proxy/scanner ambiguity. Engagement never changes delivery_status to delivered/read. Local HTTP works for testing; PUBLIC_BASE_URL determines production URLs.

## MCP and UI
Add approved-submit and outbound-status tools with explicit narrowly scoped grants, mailbox ACL, owned source credential and human-approval checks. Status includes safe identifiers, submission/delivery distinction, engagement counts and linked reply references. UI clearly separates approve and send, provides a human send action after approval, displays request-based engagement and linked replies, and updates MCP guidance and schemas.

## Verification
Isolated PostgreSQL tests cover approval without dispatch, missing/expired/revoked approval, snapshot mismatch, concurrent/idempotent dispatch, cross-tenant reads, reply sender/mailbox boundaries, token tampering, redirect target controls, booking/unsubscribe exclusion, opt-out, and proxy-sensitive labels. No tests run against the live app database. Live validation sends exactly one authorized test mail to the operator-approved test inbox after a normal audited suppression release and bounded consent. Verify Gmail arrival and headers, actual rendered links, public engagement, local booking and unsubscribe. Preserve unknown SMTP outcomes without resending. Test-only reviewer accounts are explicitly marked simulated, then disabled.

## SMTP inbox deployments and template authoring
The test account already authenticates successfully to Zoho IMAP using its stored SMTP app password. For SMTP deployments without Mail OAuth, add an explicitly enabled TLS IMAP reply monitor bound to an SMTP profile and its mailbox. Reuse the encrypted secret without exposing it. The user subsequently expanded scope to all received mail: incrementally import all messages in the configured INBOX, with bounded pages/backfill, UIDVALIDITY-aware cursors and dedupe. Leave read flags unchanged and ingest through the same untrusted inbox pipeline. Unrelated messages form independent conversations; only valid identifier/sender matches attach to outbound conversations. Poll jobs are durable, bounded and stop when the binding/profile is disabled.

The user additionally authorized MCP template creation and decoration. Add idempotent templates_create under templates.write, reuse immutable template version authoring and safe HTML validation, and document create → version → preview → draft → approval → submit. Template writing cannot authorize mail delivery.

## Full inbox MCP management (user addition)
Import all received INBOX mail through the explicitly enabled IMAP binding, not only replies. Expose mailbox-authorized paginated thread discovery/search, sanitized thread reads, idempotent local read/unread and thread status changes, and reply draft creation with existing-thread context. Require narrow scopes and current mailbox grants; hide foreign IDs and enforce optimistic versions. No permanent deletion, automatic execution of inbound instructions, or remote IMAP flag changes are included. All actual responses still require human approval.

## Campaign consistency
Apply approval/execution separation to campaign_launch as well as individual message_submit. A human-approved frozen launch remains approved without activation jobs. Explicit human execute or MCP campaign_submit_approved with campaign.submit_approved validates its exact manifest and current approval/credential/grants, then idempotently queues existing activation. Existing running/queued launches are never replayed. Agent campaign approval decisions remain forbidden.
