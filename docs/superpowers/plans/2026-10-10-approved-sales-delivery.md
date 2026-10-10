# Approved Sales Delivery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Finish approved MCP sending, status, linked replies and engagement, then validate one real owner-inbox message.

**Architecture:** Separate approval from durable dispatch through shared application services. Bind public engagement to immutable outbounds and match replies by mailbox-scoped identifiers and sender. Preserve SMTP ambiguity and human approval boundaries.

**Tech Stack:** Rust/Axum/SQLx/PostgreSQL, Next.js/React, HTTP MCP, SMTP.

**Spec:** docs/superpowers/specs/2026-10-10-approved-sales-delivery-design.md

## Global Constraints
- Human approvals remain unavailable to agents.
- Agents cannot gain mail.send or mail.approve.
- Existing queued/accepted messages are never replayed.
- Engagement never changes delivery_status to delivered/read.
- No tests run against the live app database.
- PUBLIC_BASE_URL determines production URLs.

## Review Focus
- Old queued messages and approval replay cannot send twice.
- Revoked credentials/grants/approvals stop execution before SMTP.
- Reply headers from a foreign mailbox or unexpected sender cannot attach.
- Tampered tokens and redirect query parameters cannot leak or redirect arbitrarily.
- Unsubscribe/booking remain direct and usable after tracking rewrites.

### Task 1: Approval, approved execution, MCP reads
Files: crates/application/src/approvals/mod.rs; crates/application/src/sending/mod.rs; crates/application/src/sending/dispatch.rs; crates/contracts/src/sending.rs; crates/mcp/src/tools/sending.rs; crates/mcp/src/catalog.rs; affected API routes/tests. Migration 0034 only if needed. Own isolated outreach_approved_send_verify.
- [ ] Add RED tests asserting approval leaves the outbound unqueued, agent arbitrary submit denied, approved-submit queues exactly once and safe status ACLs.
- [ ] Implement a shared approved-snapshot submit service and narrow agent scope; preserve existing human REST paths and deny arbitrary mail.send.
- [ ] Add MCP approved execution/status schema, idempotency, current approval/hash/credential/grant checks.
- [ ] Run cargo test for affected API/MCP/worker approval cases with isolated DATABASE_URL and inspect output.

### Task 2: Recipient-bound engagement
Files: new application engagement module, infrastructure engagement repository, API public engagement routes; migration 0035. Freeze hook is integrated after Task 1 releases sending/mod.rs. Own isolated outreach_engagement_verify.
- [ ] RED tests exercise opaque token validation, no open redirect, direct booking/unsubscribe, opt-out, events bounded to workspace/outbound and no false delivery status.
- [ ] Store immutable redirect destinations; rewrite only eligible content links and add a no-cache pixel at freeze using configured public origin.
- [ ] Implement public GET handlers and mailbox-authorized aggregate status.
- [ ] Run focused Rust tests and provide exact hook signature and status DTO for integration.

### Task 3: Reply threading
Files: infrastructure inbox ingest/sync and related repositories/providers; migration 0036 if needed. Sending hook is integrated after Task 1. Own isolated outreach_reply_verify.
- [ ] RED tests ingest matching reply, mismatched sender/mailbox, multi-message References and ambiguous subject-only candidates.
- [ ] Ensure outbound local threads and stable Message-ID, preserve In-Reply-To/References on send, bind inbound replies only on verified scoped match.
- [ ] Expose reply references through authorized status/thread reads and test isolation.

### Task 4: UI, docs, generated artifacts
Root owns generated OpenAPI/MCP/types and runtime. UI worker owns apps/web source/messages after final backend interfaces are available.
- [ ] Test separate approve/send controls, safe engagement labels and linked replies.
- [ ] Update help, MCP scopes/examples and deployment tracking origin documentation.
- [ ] Generate contracts with outreach-cli openapi/mcp-schema and pnpm gen:api; run web tests/lint/typecheck/build.

### Task 5: Review, integrate, actual send
- [ ] Independent read-only security/data-integrity review of approval, tracking and threading.
- [ ] Run relevant Rust suites, strict Clippy and migration upgrade checks on isolated databases.
- [ ] Back up live local database; restart only owned services with stable encryption/HMAC keys, migrate once.
- [ ] Use real MCP draft/approved-submit/status; test exact owner recipient and record actual SMTP/Gmail headers.
- [ ] Exercise open/click, booking and unsubscribe without external recipients; report Gmail evidence separately from application delivery state.
- [ ] Revoke temporary test credentials, retain audit history, commit and push verified work.

### Task 6: SMTP reply monitor and template authoring additions
- [ ] Implement opt-in TLS IMAP reply polling bound to a configured SMTP profile (migration0037); match only Office outbound identifiers, fetch bounded BODY.PEEK content, enforce dedupe and stop disabled bindings.
- [ ] Add admin enable/disable/manual sync API and worker registration; test using a local mock/provider seam without live credentials.
- [ ] Add idempotent MCP templates_create using an atomic application create_in_tx core; validate safe HTML version/preview flow and no send effects with isolated tests.
- [ ] Include these additions in the independent review and real owner-inbox validation.

### Task 7: Complete received inbox and MCP management
- [ ] Extend Task6 to all received INBOX messages using bounded UID incremental backfill, UIDVALIDITY fencing and unchanged remote flags.
- [ ] Add MCP thread list/search and local read/status management, preserving mailbox ACL/version/idempotency and human-approved responses.
- [ ] Add isolated HTTP MCP tests for scope, mailbox grants, foreign workspace IDs, replay and stale versions; label inbound content untrusted.
- [ ] Integrate SMTP inbox opt-in/configuration/manual sync and status in mail settings.

### Task 8: Campaign execution parity
- [ ] Remove campaign activation from the approval decision and retain approved launch status.
- [ ] Add human execute REST and narrow campaign_submit_approved MCP using exact manifest/hash/version checks and durable idempotency.
- [ ] Add safe campaign status MCP and UI explicit execute after approval; test missing/revoked approvals, replay/concurrency, old running launches and source credentials.
