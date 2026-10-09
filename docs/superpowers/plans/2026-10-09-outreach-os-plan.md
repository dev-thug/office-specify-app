# Outreach OS v1.0 — Implementation Plan

Spec: `docs/superpowers/specs/2026-10-09-outreach-os-design.md` (binding), source: `docs/design/source/outreach-os-design-v1.0.md`.

## Global Constraints
- Follow spec §4 shared contracts exactly; modules use them and never re-implement audit/outbox/jobs/idempotency/authz/locking.
- DTOs: `serde(deny_unknown_fields)` on requests; never accept system-owned fields (B.1.1). Provider IDs are strings.
- Every state-changing use case: one transaction containing business rows + `audit::record` + `outbox::emit` (+ `jobs::enqueue` when async).
- External effects never happen inside a request handler; they go through jobs (§12.3, §4.2 "api").
- `unknown` outcomes never auto-retry (ADR-006). Agents can never approve, grant, submit, unsuppress (§13.2, T03).
- Status enums in API must equal DDL CHECK values (§12.4).
- No raw hex/Tailwind palette classes in web; purple only in logo; UI strings via messages (spec §5).
- Tests: each module ships integration tests for its T-matrix rows (spec §7). `cargo clippy --workspace -- -D warnings` and `cargo fmt --check` clean.
- Parallel workers each work in their own git worktree on branch `feat/<task>` and only touch files they own (spec §6). Do not push.

## Wave A — foundations (parallel: Task 1 ∥ Task 2)

### Task 1: Rust foundation + database
Owner files: everything under `crates/`, `migrations/`, `Cargo.toml`, `rust-toolchain.toml`, `infra/`, `docs/design/ddl-fixes.md`, `docs/design/test-matrix.md`, `.env.example`.
1. Cargo workspace with crates per spec §3; pinned deps per spec §2; `rust-toolchain.toml` (1.99.0, rustfmt, clippy).
2. `migrations/0001_schema.sql` = appendix A.4 DDL verbatim, then fix every error found when applying to PG17 (empty DB). Record each fix (line, error, change, why it preserves intent) in `docs/design/ddl-fixes.md`. Use sqlx reversible-less migrations (single forward file each).
3. `0002_password_auth.sql` (spec §4.5: `user_password_credentials(user_id PK FK users, password_hash TEXT NOT NULL, password_changed_at, failed_attempts INT NOT NULL DEFAULT 0 CHECK >=0, locked_until TIMESTAMPTZ, created_at)`).
4. `0003_rls_roles_guards.sql`: roles/grants/RLS (spec §4.2), A.6.2 freeze guard triggers (frozen revision → no step I/U/D; frozen launch → enrollment/manifest immutable), append-only triggers for A.5 tables.
5. Shared framework spec §4.1–4.9 fully implemented and unit/integration tested: Ctx, Db/Tx (+SET LOCAL ROLE), AppError/Problem/ErrorCode (all B.5 codes), Envelope/Page/Cursor, extractors (AuthCtx, IfMatch, IdempotencyKey, CSRF), idempotency, ratelimit (Redis, fail modes), audit, outbox (+dispatcher), jobs (enqueue/claim/heartbeat/complete with fencing/retry/dead), compliance_core, email normalize, crypto (token gen, sha256, HMAC, argon2id), provider traits + Fake adapters + Zoho skeletons, ObjectStore, SecretStore, config (env), tracing JSON logs with token masking, OpenAPI aggregation via utoipa-axum, MCP JSON-RPC Streamable HTTP skeleton (initialize/ping/tools/list/tools/call dispatch to registry; protocol version `2025-11-25`; Origin allowlist; bearer service-credential auth; no stdout logs).
6. Auth (spec §4.5) end-to-end in module `org` subset: login/logout/invitation preview+accept, session+CSRF, `/me` minimal. `outreach-cli bootstrap` and `seed-demo` (synthetic data only).
7. Pre-create **all** per-module files of spec §6 for the 14 modules with empty `router()`/`register()`/`tools()` and wire them into the api/worker/mcp registries; `crates/api/tests/` harness `TestApp`; `infrastructure::testing::seed_workspace()` fixture.
8. Bins run: `outreach-api` (:8080), `outreach-worker`, `outreach-mcp` (:8081). `outreach-cli openapi > contracts/openapi/openapi.json` and `outreach-cli mcp-schema > contracts/mcp/tools.json`.
9. Tests: migrations apply on empty DB; T01 (cross-workspace → 404, RLS without setting returns 0 rows), T04/T05 (idempotency concurrency 20×, key reuse 409), lease fencing, login lockout, CSRF rejection.
Done: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo nextest run --workspace` green; `cargo run -p outreach-cli -- bootstrap …` works against outreach_dev.

### Task 2: Web foundation + design system port
Owner files: `apps/web/**` only (+ root `pnpm-workspace.yaml`, root `package.json`).
1. Next.js 16 app (pnpm), TS strict, Tailwind 4, ESLint, Vitest, Playwright config; `next.config.ts` rewrites per spec §4.10; security headers.
2. Design system port per spec §5 from `/Users/hyunjoong/Developer/specify` (read-only): `src/app/globals.css` tokens, Pretendard fonts (+OFL), primitives, plus Outreach primitives (DataTable, StatusBadge, EmptyState, ErrorState, ConflictDiff, ConfirmIrreversible). `apps/web/DESIGN.md`. Favicons from `public/logo.png`.
3. next-intl (ko default, en), messages skeleton for all §7.1 areas.
4. App shell: sidebar (logo + nav for every §7.1 route), inset panel, top bar with user menu/logout, theme (light/dark), command palette (cmdk) for navigation. Route stubs for every page in §7.1 incl. `(public)` pages, each rendering a designed placeholder + EmptyState.
5. Auth pages: `/login`, `/invite/[token]` (preview + set name/password) wired to spec §4.5 endpoints; middleware redirect to `/login` when no `outreach_session` cookie for `/app/*`.
6. API client layer: `src/lib/api/` (openapi-fetch wrapper with CSRF/Idempotency-Key/If-Match helpers, problem+json error type, TanStack Query provider). Until `contracts/openapi/openapi.json` exists, type the auth calls by hand in `src/lib/api/auth.ts`; add `pnpm gen:api` script (openapi-typescript → `src/lib/api/schema.d.ts`).
7. Lint guard script `pnpm lint:design` failing on raw hex colors / Tailwind default palette classes in `src/` (excluding globals.css).
Done: `pnpm --dir apps/web typecheck && pnpm --dir apps/web lint && pnpm --dir apps/web test && pnpm --dir apps/web build` green; screenshots of /login and /app/dashboard shell (light+dark) saved under `apps/web/test-artifacts/`.

## Wave B — backend modules (parallel, after Task 1 merged). Each: implement endpoints of spec §6 row per B.2/B.3 contracts + use cases + repo + worker handlers + tests for listed T rows.

- Task 3 `org` + `mailboxes` — T02 (grant-less admin), T03 (agent grant change), T33, A.7 last-admin rule, credential revoke stops pending jobs (§5.5).
- Task 4 `crm` — A.6.1, §5.6 activity ACL, T01 for companies, T31/T32 groundwork (address dedupe), duplicate candidates (B.3.3), B.3.4.
- Task 5 `research` + `imports` — §13.4 (≤100, no fabricated emails, basis pending), B.3.12/B.3.13, T35, CSV formula injection export, T32 (re-import keeps suppression).
- Task 6 `templates` + `sending` — §8.1 renderer, message drafts/freeze, B.3.10/B.3.11, §8.4 chain, §8.5, §8.6, A.6.4/A.6.5, T10–T16 (sending side), T13, T34.
- Task 7 `campaigns` + `approvals` — §7.5, §8.2, A.6.2, A.6.3, B.3.6–B.3.9, T06, T08, T09, T31, RECIPIENT_ACTIVE_ELSEWHERE.
- Task 8 `inbox` — §9, T07, T14, T16 (draft side), T17, classification §9.4, webhooks §14.3, attachments §15.2, T26 (sanitized HTML DTO).
- Task 9 `booking` — §10, A.6.6, B.4.2/B.4.3, T18–T22, public manage token.
- Task 10 `compliance` — §11, B.4.4, T11 (suppression side), T12, T23–T25, T32.
- Task 11 `operations` + `agents` (REST part) — jobs/retry (unknown not retryable), audit-logs redaction, dashboard §2.4 KPI definitions, agent-runs + cancel, T29/T30 worker-lock behaviour (submission workers start locked after restore flag).

## Wave C — integration (after Wave B merged)
- Task 12 MCP tools (`agents` module) — every tool in §13.2/B.6 calling existing use cases; T03, T28; schema export.
- Tasks 13–19 frontend pages (parallel, regenerate API client first): 13 dashboard+operations+settings; 14 companies+contacts; 15 inbox; 16 templates+campaigns+approvals; 17 meetings+meeting-types+public booking pages; 18 compliance + public unsubscribe; 19 agents+imports+research review.

## Wave D — verification
- Task 20 E2E Playwright §18.2 scenario + remaining T-matrix gaps; `docs/design/test-matrix.md` fully mapped; runbooks (`docs/runbooks/`: local dev, unknown reconciliation, restore).
- Final whole-branch review → one fix wave.
