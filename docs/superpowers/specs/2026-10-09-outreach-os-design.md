# Outreach OS v1.0 — Implementation Design (spec)

Status: approved by user 2026-10-09. Scope: G1–G6 complete, external providers via fake adapters.

## 0. Authority

- **Binding source:** `docs/design/source/outreach-os-design-v1.0.md` (sections 01–20 + appendix A DDL + appendix B API/MCP + C).
  The two other files in `docs/design/source/` are verbatim copies of appendices A and B.
- This spec records only **implementation decisions and deviations**. Where this spec is silent, the source doc rules.
  Where they conflict, this spec wins (only for the items listed in §1).
- Section references below such as `§8.4`, `A.6.3`, `B.5` refer to the source doc.

## 1. Deviations from the source doc (user-approved)

| # | Source says | We do | Why |
|---|---|---|---|
| X1 | OIDC SSO login (§2.2, §12.1) | **Email + password member login** (argon2id). `users.oidc_issuer/oidc_subject` stay NULL. New table `user_password_credentials`. | User decision. |
| X2 | Invitation via allowed channel | Admin creates invitation → API returns one-time invite URL once (shown in UI to copy). No email is sent. | Zoho transactional disabled (§1.2). |
| X3 | Migrations split per G-phase | All appendix-A tables created in migration 0001 | Parallel module workers must not collide on migrations; final state equals appendix A. |
| X4 | Zoho / Calendar / delivery provider real | Provider traits + `Fake*` adapters (deterministic, test-controllable) + Zoho HTTP adapter skeletons gated by config | No real accounts. Release gates Z01/C01/P01/E01 remain closed. |
| X5 | MCP OAuth discovery (§13.3) | MCP Streamable HTTP authenticated with **service credential bearer** only. OAuth protected-resource metadata endpoint returns 404; gate A01 stays closed. | No IdP. Doc forbids claiming OAuth compatibility. |

Everything else (permissions, approvals, snapshots, suppression, booking saga, idempotency, RLS, audit, outbox) is implemented as specified.

## 2. Stack (pin exact versions in lockfiles; prefer releases ≥7 days old)

- Rust stable 1.99, edition 2024. Axum 0.8, Tokio 1, SQLx 0.8 (postgres, runtime-tokio, tls-rustls, uuid, chrono, json, ipnetwork), Serde, Reqwest (rustls),
  utoipa 5 + utoipa-axum (OpenAPI from DTOs; each module router contributes its own paths), schemars (MCP tool schemas),
  argon2, sha2, hmac, rand, base64, uuid v4, chrono + chrono-tz (IANA/DST), tracing + tracing-subscriber (JSON), thiserror, anyhow (bins only),
  redis (tokio-comp) for cache/rate-limit only, ammonia (HTML sanitize), csv, idna.
- PostgreSQL 17.11 (local brew, `postgres://hyunjoong@localhost:5432/outreach_dev`, test DB via `#[sqlx::test]`), Redis 8.
- Web: Next.js 16 App Router, React 19.2, TypeScript 5.9 strict, Tailwind CSS 4.1 (`@tailwindcss/postcss`), Radix primitives, class-variance-authority,
  clsx, tailwind-merge, lucide-react, sonner, cmdk, next-intl 4 (ko default, en), TanStack Query 5, openapi-typescript + openapi-fetch, Vitest, Playwright.

## 3. Repository layout (source §4.3, rooted at repo root)

```
apps/web/                         Next.js (internal (app) + (public) route groups)
crates/contracts/                 ErrorCode, Problem, Envelope, Page, all request/response DTOs (per-module files)
crates/domain/                    pure types, state machines, normalization, hashing, renderer, slot math
crates/infrastructure/            Db/Tx, repositories (per-module files), audit, outbox, jobs, idempotency, crypto, providers, storage, secrets, redis
crates/application/               use cases per module, authz/policy, AppError
crates/api/                       Axum app, extractors, per-module routers, OpenAPI aggregation, bin `outreach-api`
crates/worker/                    job runner + per-module handlers, bin `outreach-worker`
crates/mcp/                       MCP Streamable HTTP server + per-module tools, bin `outreach-mcp`
crates/cli/                       bin `outreach-cli` (bootstrap, seed-demo, openapi export, mcp schema export)
migrations/                       sqlx migrations
contracts/openapi/openapi.json    generated
contracts/mcp/tools.json          generated
tests/e2e/                        Playwright
docs/{design,adr,runbooks}/       ddl-fixes.md, ADRs, runbooks
infra/                            local dev scripts
```

Dependency direction: `api|worker|mcp|cli → application → infrastructure → domain → contracts` (domain may depend on contracts for enums).

## 4. Shared framework contracts (built in Task 1; every module uses these and nothing else for the cross-cutting concerns)

### 4.1 Request context
```rust
pub struct Ctx {
    pub request_id: String,
    pub workspace_id: Uuid,
    pub principal_id: Uuid,
    pub principal_kind: PrincipalKind,      // Human | Agent | System
    pub role: Role,                         // Admin | Manager | Member | Agent | System
    pub scopes: ScopeSet,                   // agents/service credentials; humans get role-implied scopes
    pub permission_version: i64,
    pub auth: AuthMethod,                   // Session{session_id} | ServiceCredential{credential_id} | System{job_id}
    pub agent_run_id: Option<Uuid>,
}
```
Workspace/principal come only from the verified session/credential (§5.1). `DEFAULT_WORKSPACE_ID` server config is used for pre-auth transactions (login, public tokens, webhooks) per A.5.

### 4.2 DB / transactions / RLS
- `Db::begin(&self, workspace_id) -> Tx` runs `SELECT set_config('app.workspace_id', $1, true)` and, when configured, `SET LOCAL ROLE <runtime_role>`
  (`api_runtime` for api/mcp, `worker_runtime` for worker). Thus RLS is enforced even when the pool connects as a superuser in dev/tests.
- Migration 0003 creates NOLOGIN roles `migration_owner, api_runtime, worker_runtime, report_reader, retention_operator` if missing, enables + forces RLS on every
  table with `workspace_id` using policy `workspace_id = current_setting('app.workspace_id', true)::uuid`, grants minimal DML, and revokes DELETE/UPDATE on
  `audit_logs, approval_decisions, suppression_events, send_attempts` completed rows per A.5 (use triggers where column-level grants are insufficient).
- Pre-auth lookups on `users`/`user_password_credentials` (no workspace_id) go through a dedicated repository only.
- Optimistic locking: `UPDATE … WHERE workspace_id=$1 AND id=$2 AND version=$3` and `version = version + 1, updated_at = now()` set explicitly (A.2, A.6.1).

### 4.3 Errors
- `contracts::ErrorCode` = every code in B.5 plus: `VALIDATION_FAILED`(400/422), `NOT_FOUND`(404), `FORBIDDEN`(403), `CONFLICT`(409), `SCOPE_REQUIRED`(403),
  `INVALID_CREDENTIALS`(401), `ACCOUNT_LOCKED`(429), `CSRF_TOKEN_INVALID`(403), `DEPENDENCY_UNAVAILABLE`(503), `INTERNAL`(500).
- `application::AppError { code, detail, violations: Vec<Violation>, retry_after: Option<u32> }` → `application/problem+json` per B.1.2
  (`type = urn:outreach:problem:<kebab-code>`, `title`, `status`, `code`, `detail`, `instance`, `request_id`, `retryable`, `violations`).
- Never 500 for policy/state violations (§12.2).

### 4.4 HTTP conventions (api crate)
- Prefixes: `/api/v1` internal, `/public/v1` public, `/webhooks/v1` webhooks, `/mcp` MCP (separate bin, same lib).
- Success envelope `{ "data": …, "meta": { "request_id": … } }`; lists add `"page": { "next_cursor", "has_more" }`.
- Extractors: `AuthCtx` (session cookie `outreach_session` or `Authorization: Bearer osk_…` service credential), `IfMatch(i64)` (missing → 428 `VERSION_PRECONDITION_REQUIRED`),
  `IdempotencyKey(String)` (≤128 chars), `Json<T>` with `deny_unknown_fields` DTOs and 1MB body limit.
- Responses for versioned entities set `ETag: "<version>"`.
- Idempotency: `infrastructure::idempotency::run(tx_factory, scope, request_hash, f)` implementing B.1.3 (same key+hash → stored response; same key, different hash → 409
  `IDEMPOTENCY_KEY_REUSED`; in progress → 409 `REQUEST_IN_PROGRESS`). Scope = `(workspace, principal | public capability id, METHOD + canonical path, key)`. 72h retention.
- Pagination: `Cursor` signed with HMAC (server secret), binds sort fields + id + filter hash + principal; default limit 50, max 200 (B.1.5).
- CSRF: session-authenticated non-GET requests require header `X-CSRF-Token` equal to cookie `outreach_csrf` (double submit, cookie not HttpOnly, SameSite=Lax).
  Bearer requests are exempt. Session cookie: HttpOnly, Secure (except `APP_ENV=dev`), SameSite=Lax, Path=/.
- Public endpoints send `Cache-Control: no-store`, `Referrer-Policy: no-referrer`, `X-Robots-Tag: noindex`; tokens in URLs are masked in logs (§7.6).
- Rate limits via Redis (`infrastructure::ratelimit`), fail **closed** for public booking/unsubscribe-abuse/login and sending, fail open for CRM reads (§16.3).

### 4.5 Auth endpoints (X1)
- `POST /api/v1/auth/login {email, password}` → sets session + csrf cookies, returns `/me` payload. 5 failures → `locked_until = now()+15m` (`ACCOUNT_LOCKED`). Generic `INVALID_CREDENTIALS` message.
- `POST /api/v1/auth/logout` → revokes session.
- `GET /api/v1/auth/invitations/{token}` → invitation preview (email, role, expiry) — token hashed lookup.
- `POST /api/v1/auth/invitations/{token}/accept {display_name, password}` → creates user + password credential + human principal, marks invitation accepted, logs in.
- Password policy: ≥12 chars, ≤128, argon2id (m=19456, t=2, p=1).
- Every authenticated request re-checks session not revoked/expired and principal `active` (§5.5); `permission_version` change invalidates Redis permission cache.
- `outreach-cli bootstrap --workspace-name --admin-email --admin-password-stdin` creates workspace (emergency_stop=true), admin user+principal, system worker principal (A.7).

### 4.6 Authorization (§5.2–5.4)
`application::authz` exposes:
- `require_human(ctx)`, `require_role(ctx, &[Role])`, `require_scope(ctx, Scope)`, `require_not_agent_for(ctx, action)`
- `mailbox_grant(tx, ctx, mail_account_id) -> MailboxGrant { can_read, can_compose, can_send, can_approve, can_assign }` (agents: from `mail_account_grants` too; admins get nothing implicit)
- `require_mailbox(tx, ctx, mail_account_id, MailboxPerm)` → `MAILBOX_ACCESS_DENIED` (404 when existence must be hidden).
- `crm_can_edit(ctx, owner_principal_id)` (Member: own or unassigned-new only).
Scopes (agents/credentials): `crm.read crm.write crm.propose research.propose research.write templates.read templates.write mail.read mail.compose campaign.compose campaign.pause booking.read booking.propose approvals.read operations.read agents.run`. Humans have scopes implied by role. Agents never get approve/grant/send.

### 4.7 Audit, outbox, jobs (same transaction as the business change)
- `audit::record(tx, AuditEntry { actor_principal_id, requested_by, approved_by, job_id, action, target_type, target_id, outcome, metadata(allowlisted, no bodies/tokens) })`
- `outbox::emit(tx, event_type, aggregate_type, aggregate_id, payload)`; event types per §16.2.
- `jobs::enqueue(tx, kind, payload_version, payload, available_at, dedupe_key: Option<&str>) -> job_id`.
- Worker runner: claim per A.6.7 (SKIP LOCKED, batch 20, lease 60s, heartbeat), complete/fail with `WHERE lease_token = $token` (fencing), retry: max 5, exponential backoff + jitter, `dead` state; handlers flagged `external_effect=true` never auto-retry after the submitting boundary (§16.4). Outbox dispatcher job + `processed_events (consumer, event_id)` dedupe.

### 4.8 Shared compliance core (used by sending, inbox, booking, compliance)
`infrastructure::repo::compliance_core`:
- `lock_email_address(tx, ws, email_address_id)` — the single lock-order rule of A.6.4: `SELECT … FROM email_addresses WHERE … FOR UPDATE` before any suppression/send-permit decision.
- `active_suppressions(tx, ws, email_address_id, purpose) -> Vec<Suppression>`
- `insert_suppression(tx, ws, email_address_id, purpose_scope, reason, source, actor) -> id` (+ `suppression_events` row + outbox `suppression.created` + job to pause active enrollments).
- `issue_unsubscribe_token(tx, ws, email_address_id, outbound_message_id) -> RawToken` (256-bit, stores hash only).
- `valid_communication_basis(tx, ws, email_address_id, purpose, jurisdiction) -> Option<Basis>`.

`domain::email_address::normalize(raw) -> NormalizedEmail { original, normalized_key, normalization_version }` (§6.3: trim, domain lowercase + IDNA, local-part case policy fixed per workspace setting, no +tag/dot stripping).

### 4.9 Providers (X4)
Traits in `infrastructure::providers`:
- `MailProvider`: `list_messages(cursor) / get_message / send(SendRequest) -> SendOutcome{Accepted{provider_message_id}, Rejected{permanent, code}, Unknown}` / `lookup_sent(correlation)` / `capabilities()`.
- `CalendarProvider`: `free_busy(host, range) / create_event / update_event / cancel_event` each returning `Ok | Failed | Unknown`.
- `DeliveryProvider` (marketing/transactional): same send shape + `capabilities{one_click_unsubscribe, custom_headers, idempotency}`.
- `ObjectStore` (local FS under `var/objects`, signed short-lived download URLs), `SecretStore` (AES-GCM encrypted file under `var/secrets`, key from env).
- `Fake*` adapters keep state in PostgreSQL tables? **No** — in-memory per process plus a `FAKE_PROVIDER_SCRIPT` hook so tests can force accepted/rejected/unknown/timeout. Fake mail inbound messages are injected via a dev-only endpoint `POST /api/v1/dev/fake-mail/inbound` (compiled only with `APP_ENV=dev|test`).
- Channel policy: `delivery_channels` with `provider = zoho_mail` and purpose `marketing|transactional` can never be enabled (T33). Fake delivery channel enabling still requires an append-only `channel_reviews` row with conclusion `allowed`.

### 4.10 Next.js ↔ API
- Browser talks only to Next.js origin. `next.config.ts` rewrites `/api/v1/*`, `/public/v1/*`, `/webhooks/v1/*` to `API_ORIGIN` (default `http://127.0.0.1:8080`); cookies flow same-origin.
- Server Components fetch the API with the incoming cookie forwarded; never cache authenticated responses (`cache: 'no-store'`).
- Generated typed client `apps/web/src/lib/api/schema.d.ts` from `contracts/openapi/openapi.json` (openapi-typescript), wrapper `apiClient` (openapi-fetch) that injects `X-CSRF-Token`, `Idempotency-Key`, `If-Match` helpers and maps problem+json to typed errors.
- `allowed_actions` only hide/show buttons; server re-checks.

## 5. Design system (user requirement: logo + specify.app design system)

Port Specify's design system (`/Users/hyunjoong/Developer/specify`, read-only source) into `apps/web`:
- Tokens: copy the foundation/semantic/`@theme inline` layers of `src/app/globals.css` (OKLCH, neutral hue-290 ramp, ink `primary`, graphite `brand`, status
  `success/info/teal/amber/rose`, sidebar/app-shell tokens, radii, shadows, type scale, dark theme). Drop Specify-only editor/landing/backdrop rules.
- Fonts: Pretendard Variable dynamic subset + residual (OFL — copy `OFL.txt`), JetBrains Mono fallback stack. No brand font (logotype is the image).
- Primitives: port `src/components/ui/*` (button, badge, card, input, textarea, label, select, checkbox, switch, tabs, dialog, alert-dialog, sheet, popover,
  dropdown-menu, tooltip, command, separator, skeleton, spinner, avatar, scroll-area, sonner, alert, collapsible, accordion) keeping variants/sizes;
  add Outreach-specific primitives: `DataTable` (cursor pagination, selection vs "all filtered"), `StatusBadge` (maps every domain status to a status tone, never
  color-only — icon + text), `EmptyState`, `ErrorState`, `ConflictDiff` (412 three-way view), `ConfirmIrreversible` (shows recipients/sender/content/count).
- Rules (from Specify DESIGN.md): **purple appears only in the logo**; primary actions = ink `primary`; signals/focus = `brand`; pill controls, list rows 10px,
  cards 16px, modals 20px radii; shadows only for floating layers; no hover lift; 13–14px UI text; neutral-first; no raw hex / Tailwind palette classes.
- Shell: ChatGPT/Codex layout — borderless 260px sidebar (52px header with logo + "Outreach OS", 36px nav rows for the 13 areas of §7.1), main = inset panel 8px.
- Logo: `apps/web/public/logo.png` (user-provided, 480×480). Generate favicons (`favicon.ico`, `icon.png`, `apple-icon.png`) from it.
- i18n: all UI strings via `apps/web/messages/{ko,en}.json`; Korean default.
- Accessibility: keyboard nav, labels, focus return, status not color-only (§7.2).
- `apps/web/DESIGN.md` documents the ported system for agents (adapted from Specify DESIGN.md).

## 6. Module ownership (parallel workers own disjoint files)

Each module `<m>` owns: `crates/contracts/src/<m>.rs`, `crates/domain/src/<m>.rs`, `crates/infrastructure/src/repo/<m>.rs`, `crates/application/src/<m>/`,
`crates/api/src/routes/<m>.rs`, `crates/worker/src/handlers/<m>.rs`, `crates/mcp/src/tools/<m>.rs`, `crates/api/tests/<m>_*.rs`.
Task 1 pre-creates every one of these files (empty router/register fns) and registers them, so module workers never edit shared registries.

| Module key | Endpoints (B.2) / responsibilities |
|---|---|
| `org` | `/me`, principals, member-invitations, agents, credentials, revoke, disable-requests, emergency-stop, auth (§4.5) |
| `mailboxes` | mail-accounts, connection-requests, zoho callback (skeleton), reconnect, grants, delivery-channels (+reviews, patch), calendar-connections |
| `crm` | companies (+archive, merge-requests, activities), contacts (+emails, primary-email), notes, tasks; activity ACL filtering (§5.6) |
| `research` | lead-proposals (+decisions), research-evidence (+reviews) |
| `imports` | imports upload-requests, imports, rows, commit, exports (CSV formula-injection protection) |
| `templates` | email-templates, versions, render (allowlist renderer, §8.1) |
| `campaigns` | campaigns, revisions, preflight, launches (freeze job, manifest hash), launch get/pause/cancel, A.6.2 guards |
| `approvals` | approval-requests (campaign launch, outbound, booking change, suppression release), approvals list/get/decisions/revoke, A.6.3 |
| `sending` | message-drafts (+freeze), outbound preflight/approval-requests/submit/get/cancel, reply claims (A.6.5), send worker with §8.4 chain, unknown reconciliation, delivery_events |
| `inbox` | email-threads list/get/assignment/read-state/patch/reply-drafts, classification-reviews, attachments download, mail sync worker, webhooks ingress |
| `booking` | meeting-types, availability, booking-links, bookings, change proposals, reschedule/cancel, public slots/booking/manage, hold expiry, confirm saga |
| `compliance` | communication-bases (+reviews, revoke), suppressions (+release-requests), public unsubscribe GET/POST, RFC 8058 one-click endpoint |
| `agents` | agent-runs (+cancel), agent_actions recording helper, MCP server framework + all MCP tools (B.6) |
| `operations` | jobs get/retry-requests, audit-logs, dashboard/summary (§2.4 definitions), alerts view |

## 7. Testing

- Rust: `cargo nextest run --workspace`. DB tests use `#[sqlx::test(migrations = "../../migrations")]` with `DATABASE_URL=postgres://hyunjoong@localhost:5432/outreach_test`.
  `infrastructure::testing` provides fixtures: `seed_workspace() -> TestWorld { admin, manager, member_a, member_b, agent, system, mailbox, … }`, `ctx_for(principal)`,
  and an in-process Axum test client (`api::testing::TestApp`) with login helpers.
- Every T01–T36 (§18.1) maps to at least one named test `t01_…` in the owning module's tests; the matrix lives in `docs/design/test-matrix.md`.
- A.8 DB tests (FK violations, RLS without setting, EXCLUDE+DST, lease fencing) in `crates/infrastructure/tests/`.
- Web: `pnpm --dir apps/web typecheck lint test`; Playwright E2E for §18.2 against `outreach-api` + `outreach-worker` + `next start` with fake providers.

## 8. Explicitly not done (state in final report)
Real Zoho/Calendar/delivery calls, OIDC, MCP OAuth discovery, load tests, backup/restore drill, legal review — gates L01/P01/Z01/Z02/C01/E01/A01/O01/S01 remain closed.
