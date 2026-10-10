# Personalized sales kanban implementation plan

> **For agentic workers:** Use superpowers:subagent-driven-development or superpowers:executing-plans to implement task by task.

**Goal:** Organize company-specific outreach drafts, human review and ten-minute sending batches on one campaign board.

**Architecture:** Add a personalized campaign grouping module around existing CRM, templates and immutable sending. Retain bulk campaigns separately. Queue through the existing durable dispatcher, and derive result columns from its records.

**Tech Stack:** Rust/Axum/SQLx/PostgreSQL, MCP, Next.js/React Query, existing email editor.

**Spec:** `docs/superpowers/specs/2026-10-10-personalized-sales-kanban-design.md`

## Global constraints

- Keep independent human approval, exact payload binding, source credential, suppression and unknown-send protections.
- Batch at the next 600-second epoch boundary; refresh board every two seconds.
- No live test email; use disposable databases/fake SMTP and local unsent/canceled cards.
- Root owns registration, generated files and live database; workers preserve each other's changes and do not spawn descendants.

## Review focus

1. Foreign or stale target/draft/outbound linkage must not send or leak content.
2. Human/agent permissions and private draft visibility must hold at each read/write.
3. Retrying after a ten-minute boundary must not create a new job or schedule.
4. Default sender changes/disable must not silently retarget groups.
5. Client drag state must never fabricate approval, acceptance, or safe retry of unknown mail.

### Task 1: Backend campaign grouping and default sender

Files: migration 0042, new contracts/application/infrastructure repo/API route `sales_campaigns` modules and API tests. Root registers exports/routes/OpenAPI.

- [ ] Add failing isolated tests for two companies/different drafts, CRM target validation, mailbox scope, stable sender binding, stale target pointer and derived status.
- [ ] Implement bounded group/default-sender/target/stage/draft/queue endpoints with idempotency and audit; call existing sending functions in the same transaction.
- [ ] Queue computes next boundary inside outer idempotency; validate approved outbound belongs to current target draft.
- [ ] Verify same-key boundary replay, unapproved queue denial, duplicate requests, foreign IDs, disabled defaults, accepted/unknown columns and no automatic retry.
- [ ] Run focused API tests and scoped formatting; report DTO/function contracts to dependent workers.

### Task 2: Sending scheduling adapter and MCP

Files: sending application adapter, new MCP `sales_campaigns` tools and integration tests. Root registers tool module/catalog.

- [ ] Add meaningful regression that scheduled approved submission waits until `not_before`, preserves approval/source credential and blocks unapproved/foreign/unknown cases.
- [ ] Expose `submit_scheduled_approved_in_tx` exactly as in the spec using existing `submit_tx`.
- [ ] Implement MCP list/create/get/add-target/create-message/move-stage/queue tools using Task 1 use cases, narrow scopes and stable idempotency keys.
- [ ] Test actual MCP calls for two company drafts, required human handoff and one approved queued job. Never grant agent approval/direct-send scope.

### Task 3: Campaign board, detail and sender Settings

Files: new personalized campaign components/API wrapper/tests, primary campaign routes, explicit bulk routes, SMTP settings UI and locale copy. Root owns generated schema.

- [ ] Test simplified group creation without mailbox/channel selectors, missing default Settings link, company/contact target selection, independent draft bodies and preview.
- [ ] Build columns, card dialog with CRM sources/editor/rendered recipient preview and existing send/approval controls.
- [ ] Support drag/drop and buttons; server-confirm all changes, explain approval blocks, show next batch time, poll every two seconds and distinguish provider acceptance/attention.
- [ ] Add default-sender action in SMTP Settings, and preserve existing bulk campaign screens under labeled routes.
- [ ] Run focused/full relevant web tests, lint, typecheck and production build with API_ORIGIN set for the local test server.

### Task 4: Root integration and independent review

- [ ] Register modules/routes/OpenAPI/MCP tools and regenerate contracts.
- [ ] Review authorization, idempotency, batching and immutable draft bindings independently; fix findings before completion.
- [ ] Run targeted Rust/MCP tests, strict Clippy, web checks and builds. Backup/migrate local database with stable secret/HMAC configuration.
- [ ] Demonstrate one group with two company drafts, recipient preview, guarded queue movement and status refresh. No real send.
- [ ] Update README/MCP usage, remove temporary test credentials, commit/push after validation and report concrete results/limits.
