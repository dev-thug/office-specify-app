# Office Zoho Campaigns Implementation Plan

> **Retired historical plan:** this plan describes a native Zoho Campaigns
> integration that has since been retired. Keep it for design history; do not
> execute it as current work. Office campaigns now use SMTP, and Office handles
> unsubscribe and booking links locally. See [current integration
> status](../../design/zoho-integration-status.md) and the [SMTP runbook](../../runbooks/smtp.md).

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Office의 승인된 단일 단계 마케팅 캠페인을 Zoho Campaigns 무료 계정에 연결하고, 동의한 연락처 동기화·즉시 발송·결과 폴링을 지원한다.

**Architecture:** Zoho Campaigns는 별도 bulk provider로 구현한다. 기존 승인·캠페인 launch·동의/수신거부 모델을 재사용하되 수신자별 dispatch와 bulk dispatch는 배타적으로 선택한다. 외부 작업 journal과 영속 provider key를 기준으로 불명 상태를 조회하고 재발송하지 않는다.

**Tech Stack:** Rust 1.99 workspace, Axum, SQLx/PostgreSQL, reqwest, SecretStore, durable jobs/outbox, Next.js 16/React 19/TypeScript, Vitest, Playwright.

**Spec:** `docs/superpowers/specs/2026-10-09-office-zoho-campaigns-design.md` — 사용자 승인 2026-10-09.

## Global Constraints

- 사용자 계정은 `https://campaigns.zoho.com/`, Campaigns 무료 플랜, Zoho Mail 유료 플랜이다.
- 첫 구현은 US DC(`accounts.zoho.com`, `campaigns.zoho.com`), 단일 단계, 즉시 발송이다. 예약·웹훅·자동화는 비활성화한다.
- 기존 `zoho_mail` 마케팅 금지를 유지한다. 실제 Mail 수신/회신 어댑터를 이번 범위에 포함하지 않는다.
- 새로운 외부 요청은 검증된 고정 호스트, HTTPS, 리다이렉트 금지, 제한된 응답 크기/시간으로 실행한다.
- 토큰은 SecretStore에 암호화 저장하고 API 응답·Debug·로그·감사·idempotency 응답에 넣지 않는다.
- 동의 없는 연락처, 수신거부 주소, 확인 대기 연락처를 발송 대상에 포함하지 않는다.
- 에이전트는 승인·최종 발송을 할 수 없다. 승인·대상·내용 hash와 현재 사람의 권한을 검사한다.
- 외부 생성/발송 타임아웃은 unknown이며 무조건 재시도하지 않는다. HTTP 200만으로 성공을 판단하지 않는다.
- `content_url`은 외부 HTTPS에서 접근 가능해야 한다. localhost에서 콘텐츠 배포/실발송을 활성화하지 않는다.
- 적용된 마이그레이션을 편집하지 않는다. 새 테이블은 workspace 복합 FK/RLS와 최소 역할 권한을 적용한다.
- 무료 계정의 실제 연락처/월 발송 한도는 계정에서 확인해 기록하며 확인되지 않은 수치를 하드코딩하지 않는다.

## Review Focus

1. 동일 주소가 여러 연락처에 연결되거나 두 launch가 동시에 동작해도 대상·개인화가 섞이지 않아야 한다 — Task 6.
2. Office 승인 뒤 Zoho에서 목록/본문이 변경되면 발송을 차단해야 한다 — Task 7.
3. API/worker가 동시에 토큰을 갱신하거나 SecretStore를 갱신해도 비밀 파일이 유실·손상되지 않아야 한다 — Task 3.
4. 공급자가 요청을 처리한 직후 응답/worker가 사라져도 중복 생성·발송하면 안 된다 — Task 7.
5. 무료 플랜과 공개 HTTPS 미설정 상태에서 미지원 기능을 성공한 것처럼 보여주면 안 된다 — Tasks 4, 8, 9.

## 파일과 책임

| 위치 | 책임 |
|---|---|
| `crates/domain/src/marketing.rs` | 기능 제한, 상태 전이, merge mapping 검증 |
| `crates/contracts/src/marketing.rs` | 비밀정보 없는 HTTP/MCP DTO |
| `crates/infrastructure/src/providers/marketing.rs` | bulk provider trait·결과 타입 |
| `crates/infrastructure/src/providers/zoho_campaigns/` | 인증, HTTP 파싱, 연락처/캠페인/보고서 API |
| `crates/infrastructure/src/repo/marketing.rs` | workspace-scoped 영속 매핑·journal |
| `crates/application/src/marketing/` | OAuth, 연결 설정, export, 승인·발송, reports |
| `crates/api/src/routes/marketing.rs` | 인증된 관리/캠페인 API와 public callback/content |
| `crates/worker/src/handlers/marketing.rs` | 영속 작업·외부 요청·결과 확인 |
| `apps/web/src/features/marketing/` | 연결 설정과 캠페인 동기화/검토/발송 상태 |
| `apps/web/src/lib/api/marketing.ts` | OpenAPI 기반 호출과 query keys |
| `migrations/0018_zoho_campaigns.sql` | 공급자 제약 확장·새 테이블·RLS·권한 |
| `docs/runbooks/zoho-campaigns.md` | OAuth 클라이언트 설정·계정 검증·복구 |

Task 2의 확인 결과와 Task 1의 인터페이스를 먼저 고정한다. 이후 구현은 순차 진행을
권장한다. OAuth/설정/worker가 같은 연결과 journal을 사용하므로 독립 대규모 변경보다
작은 단위의 테스트·커밋이 적합하다.

## Task 1 — 도메인·데이터 계약과 마이그레이션

**Files:** Create `crates/domain/src/marketing.rs`, `crates/contracts/src/marketing.rs`,
`crates/infrastructure/src/repo/marketing.rs`, `migrations/0018_zoho_campaigns.sql`,
`crates/infrastructure/tests/marketing.rs`. Modify 각 crate `src/lib.rs`,
`crates/domain/src/mailboxes.rs`, `crates/infrastructure/src/providers/mod.rs`.

**Interfaces:**

```rust
pub enum MarketingStatus {
    Preparing, Syncing, NeedsReview, Ready, Submitting,
    Accepted, InProgress, Completed, Rejected, Unknown, Canceled,
}
pub struct MarketingCapabilities {
    pub immediate_send: bool, pub scheduled_send: bool,
    pub webhooks: bool, pub personalized_fields_verified: bool,
}
pub fn validate_free_plan_step(step_count: usize, scheduled: bool) -> Result<(), AppError>;
pub async fn get_connection(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<MarketingConnectionRow>, AppError>;
pub async fn get_export(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<MarketingExportRow>, AppError>;
```

- [ ] Write failing domain/DB tests: free single-step immediate succeeds; scheduled/multi-step fails; cross-workspace get returns None; duplicate `(workspace_id, launch_id, step_id)` export fails; `zoho_mail` marketing remains blocked.

```rust
assert!(validate_free_plan_step(1, false).is_ok());
assert!(validate_free_plan_step(1, true).is_err());
assert!(validate_free_plan_step(2, false).is_err());
```

- [ ] Run `cargo test -p domain marketing` and `DATABASE_URL=postgres://hyunjoong@localhost:5432/outreach_codex_verify cargo test -p infrastructure --test marketing`; confirm the new contracts are absent before implementation.
- [ ] Implement migration: connections keyed by existing `integration_connections` ID; exports keyed by launch/step with snapshot/audience hashes; recipient mappings keyed by export/email ID; operation journal keyed by export/kind/correlation; report checkpoints keyed by export. All keys include workspace. `zoho_campaigns` is marketing-only. Keep payload bodies out of journal and audit metadata.

```sql
CREATE UNIQUE INDEX marketing_one_export_per_step
ON marketing_exports (workspace_id, campaign_launch_id, campaign_step_id);
CREATE UNIQUE INDEX marketing_operation_once
ON marketing_operations (workspace_id, export_id, kind, correlation_key);
```

- [ ] Run the domain/DB tests again, verify owner migration plus api/worker RLS access and report-reader denial, then commit `feat(marketing): add bulk campaign contracts and persistence`.

## Task 2 — 공식 API 계약과 HTTP 테스트 고정

**Files:** Create `docs/design/zoho-campaigns-api-contracts.md`,
`crates/infrastructure/src/providers/marketing.rs`,
`crates/infrastructure/src/providers/zoho_campaigns/{mod,http,contacts,campaigns,reports}.rs`,
`crates/infrastructure/tests/zoho_campaigns_http.rs`.

**Consumes:** Task 1 connection/export IDs and capability restrictions.
**Produces:** `MarketingCampaignProvider` with `list_lists`, `subscribe`, `unsubscribe`,
`create_campaign`, `get_campaign`, `request_send`, and `get_report` async operations.
Requests carry explicit connection context, correlation IDs, and immutable typed content;
responses distinguish Accepted/Rejected/Unknown and transport/reauth/rate-limit errors.

계약 타입은 이 task에서 정의하고 이후 task에서 같은 타입을 사용한다.

```rust
pub struct ProviderCampaignDraft {
    pub correlation_id: String, pub name: String, pub subject: String,
    pub from_email: String, pub from_name: String, pub list_key: String,
    pub topic_id: String, pub content_url: String,
}
pub struct ProviderContact {
    pub email: String, pub fields: std::collections::BTreeMap<String, String>,
    pub topic_id: String,
}
pub enum SendCampaignResult {
    Accepted { campaign_key: String, status: String },
    Rejected { code: String },
    Unknown,
}
pub struct ProviderList { pub key: String, pub name: String }
pub struct ProviderSubscription { pub confirmed: bool, pub status: String }
pub struct ProviderCampaign {
    pub key: String, pub status: String,
    pub content_hash: Option<String>, pub recipient_hash: Option<String>,
}
pub struct ProviderRecipientEvent {
    pub event_id: String, pub email: String, pub kind: String,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
}
pub struct ProviderReport {
    pub totals: std::collections::BTreeMap<String, u64>,
    pub recipient_events: Vec<ProviderRecipientEvent>, pub next_cursor: Option<String>,
}
#[async_trait::async_trait]
pub trait MarketingCampaignProvider: Send + Sync {
    async fn list_lists(&self) -> Result<Vec<ProviderList>, ProviderError>;
    async fn subscribe(&self, list_key: &str, contact: ProviderContact) -> Result<ProviderSubscription, ProviderError>;
    async fn unsubscribe(&self, list_key: &str, email: &str, topic_id: &str) -> Result<(), ProviderError>;
    async fn create_campaign(&self, draft: ProviderCampaignDraft) -> Result<SendCampaignResult, ProviderError>;
    async fn get_campaign(&self, key: &str) -> Result<ProviderCampaign, ProviderError>;
    async fn request_send(&self, key: &str) -> SendCampaignResult;
    async fn get_report(&self, key: &str, cursor: Option<&str>) -> Result<ProviderReport, ProviderError>;
}
```

`ProviderError`는 기존 `providers::ProviderError`를 사용한다. provider 인스턴스는
검증한 connection ID에 고정하고 다른 workspace의 토큰을 받지 않는다. 공식 API가
제공하지 않는 content/audience hash와 개인별 이벤트는 None/빈 배열로 유지한다.


- [ ] Record official request/response fixtures for US v1.1. Known paths: `getmailinglists`, `json/listsubscribe`, `json/listunsubscribe`, `createCampaign`, `sendcampaign`, `campaignreports`. Confirm topic/sender lookup, list creation, campaign status/review and recipient reports before adding a path. Unavailable capabilities remain explicit manual verification states; do not guess endpoints.
- [ ] Write HTTP tests using a local Axum mock server. Assert form encoding for Korean text/JSON lists, `Authorization: Zoho-oauthtoken …`, response envelope variants, numeric/string success codes, and no follow of redirects.

```rust
#[test]
fn send_success_is_not_inferred_from_http_200() {
    let result = parse_send_response(200, br#"{"response":{"code":"6611","message":"Content is not reviewed"}}"#);
    assert!(matches!(result, SendCampaignResult::Rejected { .. }));
}
```

- [ ] Run `cargo test -p infrastructure --test zoho_campaigns_http` and confirm failure, then implement parsers and bounded HTTP requests. `parse_send_response(status: u16, body: &[u8]) -> SendCampaignResult` accepts documented code 200 only; review/changed-recipient/content errors become actionable blocked states.
- [ ] Test 401, 429 with Retry-After, malformed JSON, HTTP 200/business error, 5xx and timeout. Creation/send timeout maps to Unknown; read-only request failure remains retryable. Scrub provider messages before returning/logging them.
- [ ] Commit `feat(marketing): implement verified Zoho Campaigns HTTP contracts`.

## Task 3 — SecretStore와 OAuth 연결

**Files:** Modify `crates/infrastructure/src/secrets.rs`, `crates/infrastructure/src/config.rs`,
`crates/api/src/state.rs`, `crates/worker/src/lib.rs`, `.env.example`.
Create `crates/application/src/marketing/{mod,oauth,connections}.rs`,
`crates/infrastructure/tests/secret_store_concurrency.rs`, `crates/api/tests/marketing_oauth.rs`.

**Interfaces:** `begin_connection(db, ctx, config) -> OAuthStartResponse`,
`complete_connection(db, ctx, state, code) -> ConnectionResponse`,
`access_token(connection_id) -> SecretAccessToken`. `SecretAccessToken` has no raw Debug
or Serialize implementation. HTTP adapters receive tokens through the provider context.

- [ ] Write failing tests for callback state expiry/reuse, wrong workspace/principal, disabled principal and agent denial. A missing client configuration returns a safe setup-needed response with callback URI, without attempting network access.
- [ ] Add a two-instance SecretStore concurrency test: concurrent puts to different names preserve both; interrupted writes leave a readable prior file; mode is 0600. The current per-instance mutex alone cannot serialize API and worker processes.

```rust
let (a, b) = tokio::join!(store_a.put("connection:a", "token-a"), store_b.put("connection:b", "token-b"));
a.unwrap(); b.unwrap();
assert_eq!(store_a.get("connection:b").await.unwrap().as_deref(), Some("token-b"));
```

- [ ] Run the new tests to observe failure. Implement cross-process exclusive file locking around read/modify/write and an atomic 0600 temporary-file replacement. Use the pinned Rust toolchain's file-lock API; keep blocking lock acquisition off async executor threads.
- [ ] Implement OAuth with US fixed host pair, state hash/expiry/single-use consumption and exact callback URI. Configure `ZOHO_CAMPAIGNS_CLIENT_ID`, `ZOHO_CAMPAIGNS_CLIENT_SECRET`, `ZOHO_CAMPAIGNS_REDIRECT_URI`; secrets never enter database metadata or serialized DTOs.
- [ ] Serialize token refresh per connection. Preserve existing refresh token if the token endpoint omits a new one. Invalid grant marks reauth-required and blocks queued operations; test API and worker concurrent refresh and secret file failures.
- [ ] Run `cargo test -p infrastructure --test secret_store_concurrency` and `cargo test -p api --test marketing_oauth`, then commit `feat(marketing): add OAuth connection and safe shared credential storage`.

## Task 4 — 연결 설정 API·화면

**Files:** Create `crates/api/src/routes/marketing.rs`, `apps/web/src/lib/api/marketing.ts`,
`apps/web/src/features/marketing/connection-settings.tsx`,
`apps/web/src/features/marketing/__tests__/connection-settings.test.tsx`.
Modify `crates/api/src/routes/mod.rs`, `apps/web/src/features/settings/mail-accounts-screen.tsx`,
`apps/web/messages/{en,ko}/settings.json`.

**Routes:** `GET /marketing/connections`, `POST /integrations/zoho-campaigns/connect`,
`GET /integrations/zoho-campaigns/callback`, `POST /marketing/connections/{id}/verify`,
`POST /marketing/connections/{id}/disconnect`. Mutations use CSRF/idempotency and
human-admin authorization. List/status responses never return tokens.

- [ ] Write API tests for human admin versus manager/agent/bearer, missing client configuration, replay and foreign connection IDs. Write UI test for the free-plan badge, disabled schedule/webhook options and required callback setup.
- [ ] Run `cargo test -p api --test marketing_oauth` and `pnpm --dir apps/web exec vitest run src/features/marketing/__tests__/connection-settings.test.tsx` to observe failure.
- [ ] Implement connect/reconnect/disconnect and verification status. Disconnect blocks queued work before revoking local credentials. Display account host, sender/topic validation, content-origin status and last checked time. Zoho Mail paid status does not upgrade Campaigns capabilities.

```tsx
<Button disabled={!connection.capabilities.scheduled_send}>예약 발송</Button>
```

- [ ] Regenerate OpenAPI/web schema with `cargo run -p cli -- openapi` and `pnpm --dir apps/web gen:api`; run API/UI tests plus typecheck and commit `feat(marketing): add Campaigns connection settings`.

## Task 5 — 불변 HTML 콘텐츠 전달

**Files:** Create `crates/application/src/marketing/content.rs`,
`crates/api/tests/marketing_content.rs`. Modify marketing repository and API route module.

**Interfaces:** `issue_content_capability(tx, export_id, content_hash, expires_at) -> SecretContentUrl`,
`read_content(db, raw_capability) -> ApprovedMarketingHtml`. Route:
`GET /public/v1/marketing-content/{token}`; no ordinary workspace session is required.

- [ ] Write tests for missing/expired/revoked token, changed content hash, allowed repeated fetch, no recipient PII/token in HTML, noindex/no-store/CSP, and localhost origin rejection during export preparation.
- [ ] Run `cargo test -p api --test marketing_content` to observe failure.
- [ ] Implement high-entropy capability hashing and bounded response. Store immutable content in ObjectStore. Publish only approved bulk HTML with validated Zoho merge tags; never publish individually rendered CRM mail.

```rust
if !content_origin.starts_with("https://") {
    return Err(AppError::validation("public HTTPS content origin required", vec![]));
}
```

- [ ] Validate URL with the shared structured URL validator in addition to the scheme check; redact token paths and OAuth query parameters from access/error logs. Test that raw token and grant code are absent in captured logs.
- [ ] Run tests and commit `feat(marketing): add bounded immutable content delivery for Zoho`.

## Task 6 — 대상 스냅샷·연락처 동기화·개인화

**Files:** Create `crates/application/src/marketing/{export,audience,personalization}.rs`,
`crates/worker/src/handlers/marketing.rs`, `crates/worker/tests/marketing_sync.rs`.
Modify `crates/application/src/campaigns/{preflight,freeze}.rs`, worker registry and marketing repository.

**Interfaces:** `prepare_export(tx, ctx, launch_id) -> ExportResponse`,
`sync_recipients(context, export_id) -> SyncSummary`;
`SyncSummary` separates requested, confirmed, pending_confirmation, excluded and failed counts.

- [ ] Write failing tests for duplicate shared addresses, revoked consent, suppression after snapshot, partial synchronization, confirmation-required response and two simultaneous exports touching the same personalized contact.
- [ ] Run `cargo test -p worker --test marketing_sync` to observe failure.
- [ ] Create a provider list dedicated to the export using the verified Task 2 contract. If creation cannot be verified, require an explicit empty dedicated list and block shared/populated list use; do not treat a shared current list as an approved audience.
- [ ] Compute audience/content/field-map hash. Normalize addresses and require explicit marketing consent. Store provider subscription outcomes and confirm membership through the verified read API before making an export ready.

```sql
SELECT email_address_id FROM marketing_recipients
WHERE workspace_id=$1 AND export_id=$2 AND status='confirmed'
ORDER BY email_address_id;
```

- [ ] Lock active per-contact personalizations or use verified isolated mapping; reject conflicting simultaneous exports. Compile only validated merge tags. Unsupported placeholders, per-recipient booking URLs without verified mapping, and manual overrides of system unsubscribe URLs block preflight.
- [ ] Test that personal field mapping changes invalidate the approval hash. Run sync/API tests and commit `feat(marketing): sync consented immutable campaign audiences`.

## Task 7 — 승인·단일 발송·unknown 복구

**Files:** Create `crates/application/src/marketing/{dispatch,reconcile}.rs`,
`crates/worker/tests/marketing_dispatch.rs`. Modify campaign activation/scheduler,
approval side-effect routing, worker marketing handlers and marketing API routes.

**Interfaces:** `request_marketing_send(tx, ctx, export_id) -> JobResponse`,
`dispatch_marketing(context, export_id) -> DispatchOutcome`,
`reconcile_marketing(context, export_id) -> ReconcileOutcome`.

- [ ] Write tests for same-key concurrency, different-key duplicate send, worker loss after provider acceptance, changed Zoho content/list after approval, emergency stop, restore lock, disabled sender/connection, stale approver grant and direct agent call.
- [ ] Run `cargo test -p worker --test marketing_dispatch` to observe failure.
- [ ] Route a Zoho launch exclusively to `marketing.dispatch`; assert no `sending.dispatch_outbound` jobs are created for that launch. Journal intent and correlation before the external call, fence completion and preserve provider keys across retries.

```rust
match provider.request_send(&campaign_key).await {
    SendCampaignResult::Accepted { campaign_key, status } => save_accepted(tx, export_id, campaign_key, status).await,
    SendCampaignResult::Rejected { code } => save_rejected(tx, export_id, code).await,
    SendCampaignResult::Unknown => save_unknown_and_enqueue_lookup(tx, export_id).await,
}
```

`save_accepted`, `save_rejected`, `save_unknown_and_enqueue_lookup` are repository
operations owned by this task; all require the current fenced operation token.

- [ ] Before submission, recheck exact upstream audience/content where the provider exposes it. Unverifiable or changed data blocks send and requires review. Provider review-required errors remain NeedsReview, not generic retryable failures.
- [ ] Unknown create/send never triggers a fresh create/send automatically. Reconcile by persisted provider key/correlation and verified read APIs. Ambiguity stays Unknown with an operator explanation. Run tests and commit `feat(marketing): add approval-bound bulk send and lookup-only recovery`.

## Task 8 — 보고서·양방향 수신거부

**Files:** Create `crates/application/src/marketing/reports.rs`,
`crates/worker/tests/marketing_reports.rs`. Modify compliance outbox consumers,
marketing handlers/repository, operations queues and CRM activity projection.

**Interfaces:** `poll_report(context, export_id) -> ReportSyncSummary`,
`propagate_suppression(context, suppression_id) -> SuppressionSyncSummary`.
Routes: `GET /marketing/exports/{id}`, `GET /marketing/exports/{id}/reports`,
`POST /marketing/exports/{id}/refresh` (human/scoped authorized callers, idempotent job enqueue).

- [ ] Write tests for overlapping/repeated pages, aggregate-only response, delayed unsubscribe, partial propagation, permanent bounce and an unknown/unrecognized recipient returned by Zoho.
- [ ] Run `cargo test -p worker --test marketing_reports` to observe failure.
- [ ] Poll with durable checkpoints and bounded intervals; store aggregate report separately from individually evidenced events. Never mark every recipient delivered/opened from campaign totals. Expose freshness and coverage explicitly.
- [ ] Route Office marketing suppression to Zoho unsubscribe and Zoho unsubscribe/permanent rejection to existing compliance_core. Unknown addresses are not imported into CRM. Failed unsubscribe propagation keeps affected exports blocked; no automatic resubscribe on later CRM sync.

```rust
if report.recipient_events.is_empty() {
    save_aggregate_only(tx, export_id, &report.totals).await?;
    // No per-recipient delivery/open updates without evidence.
}
```

- [ ] Run tests and commit `feat(marketing): reconcile campaign reports and suppressions`.

## Task 9 — 캠페인 화면·MCP 호환

**Files:** Create `apps/web/src/features/marketing/{campaign-provider-section,export-status}.tsx`,
`apps/web/src/features/marketing/__tests__/campaign-provider.test.tsx`,
`apps/web/e2e/zoho-campaigns.spec.ts`, `crates/mcp/tests/marketing.rs`.
Modify campaign builder/detail, `apps/web/src/lib/api/{campaigns,marketing}.ts`,
locale messages, `crates/mcp/src/tools/campaigns.rs`, OpenAPI/MCP generated schemas.

- [ ] Write tests for unavailable credentials, free-plan one-step/instant-only controls, empty topic/sender, consent exclusions, missing HTTPS origin and review-required/unknown states. Browser tests use a local provider fixture; never real contacts.
- [ ] Run targeted Vitest/API/MCP tests and the new browser test to observe failure before implementing screen changes.
- [ ] Reuse the Office visual template editor. Display exact recipient count/hash, excluded/pending counts, verified sender/topic, provider review, last refresh and actual capability restrictions. Show setup-needed actions rather than claiming connection success.
- [ ] Keep final send human-only. Add provider fields to existing MCP draft/preflight inputs only where required; no approval/send MCP tool. Test raw secret/nonexistent workspace arguments and agent scope denial.

```tsx
<ExportStatus status={exportState.status}
  confirmed={exportState.recipients.confirmed}
  pending={exportState.recipients.pending_confirmation}
  refreshedAt={exportState.report_refreshed_at} />
```

- [ ] Regenerate schemas, run typecheck/lint/design tests, targeted browser tests and commit `feat(marketing): connect Office campaign UI to Zoho Campaigns`.

## Task 10 — 계정 준비 안내·전체 검증·실행본 반영

**Files:** Create `docs/runbooks/zoho-campaigns.md`; modify README, integration contracts,
test matrix and `.env.example`. Update the approved design's implementation status without
claiming an actual account verification that did not run.

- [ ] Document US OAuth Console registration, exact callback, scopes, verified sender/topic,
public HTTPS origin and encryption key setup. Give a separate test-recipient-only checklist
for connection→list→draft→provider review→human send→report→unsubscribe.
- [ ] Document Unknown recovery, reauth-required, quotas, content URL expiry and limitations
of the free plan. Token creation/consent and the final real send remain user actions.
- [ ] Run final gates with a dedicated synthetic DB:

```sh
DATABASE_URL=postgres://hyunjoong@localhost:5432/outreach_codex_verify cargo nextest run --workspace -j4 --no-fail-fast
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo build --workspace
pnpm --dir apps/web typecheck
pnpm --dir apps/web lint
pnpm --dir apps/web lint:design
pnpm --dir apps/web test
API_ORIGIN=http://127.0.0.1:18080 pnpm --dir apps/web build
```

- [ ] Restart only this task's local API/web/worker processes with the revised config. Run
Playwright with the existing private QA credential file and provider stub. Check settings,
campaign controls, UI failure explanations and screenshots in both locales.
- [ ] Report actual test counts, mock-versus-real coverage, setup values still required,
current running URLs and remaining gates. Commit validated changes and push `HEAD:main`
to the existing `dev-thug/office-specify-app` repository after checking no secret files are staged.

## 실행 준비 및 검토

- [x] 사용자 목적·서비스·플랜·US 접속 도메인 확인.
- [x] 구조 설계 승인 및 기존 provider/FK/승인/SecretStore 구조 확인.
- [x] 작업별 파일·인터페이스·테스트·실패 처리·실제 계정 준비 단계 정의.
- [ ] 사용자 작업 계획 검토 및 실행 방식 선택.

권장 실행 방식은 **직접 순차 구현**이다. OAuth와 영속 journal을 먼저 고정하고
각 단계 테스트를 통과시킨 뒤 UI를 연결한다. 단계마다 별도 구현/검토 에이전트를
사용하는 방식도 가능하지만 검토·인계 비용이 더 크다.
