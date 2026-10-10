/**
 * campaigns API — 소스 §7.5, §8.2, A.6.2, B.2(캠페인 rows), B.3.6–B.3.8.
 * openapi.json 생성 전 손 타이핑 — 필드명은 부록 B wire 형식 그대로.
 *
 * 대상 선택 보조로 `GET /contacts`, `GET /contacts/{id}`·`GET /delivery-channels`
 * (모두 B.2 문서화 경로)를 최소 타입으로 소비한다 — contacts/settings 의 본격
 * 래퍼는 각 영역 작업자가 만든다.
 */
import { apiFetch, type ApiResult } from "./client";
import { mutate, type MutateOptions } from "./mutate";

export const CAMPAIGNS_PATH = "/api/v1/campaigns";
export const DELIVERY_CHANNELS_PATH = "/api/v1/delivery-channels";
export const CONTACTS_PATH = "/api/v1/contacts";

// ── 상태 집합 (DDL CHECK) ────────────────────────────────────────────

export type CampaignStatus =
  | "draft"
  | "awaiting_approval"
  | "active"
  | "paused"
  | "completed"
  | "canceled";

export type RevisionStatus = "draft" | "frozen";

export type LaunchStatus =
  | "building"
  | "frozen"
  | "awaiting_approval"
  | "approved"
  | "running"
  | "paused"
  | "completed"
  | "canceled";

export type CampaignLaunchExecutionStatus =
  | "not_ready"
  | "ready"
  | "queued"
  | "running"
  | "activated"
  | "failed"
  | "canceled";

export type EnrollmentStatus =
  | "pending"
  | "active"
  | "completed"
  | "stopped"
  | "suppressed";

// ── DTO ──────────────────────────────────────────────────────────────

/** schedule_rules (B.3.6) — weekdays 는 ISO 요일(1=월…7=일)로 해석한다. */
export interface ScheduleRules {
  weekdays?: number[];
  window_start?: string; // "HH:MM"
  window_end?: string;
  internal_daily_cap?: number;
  minimum_gap_seconds?: number;
  launch_not_before?: string; // RFC 3339
  launch_expires_at?: string;
  meeting_type_id?: string;
  [key: string]: unknown;
}

export interface CampaignStepInput {
  step_order: number; // 1..3 (DDL CHECK)
  template_version_id: string;
  delay_hours: number;
}

export interface CampaignStep extends CampaignStepInput {
  id?: string;
  [key: string]: unknown;
}

/** campaign_revisions 스냅샷 (§8.2). */
export interface CampaignRevision {
  id: string;
  campaign_id?: string;
  revision_no: number;
  status: RevisionStatus;
  purpose?: string;
  mail_account_id: string;
  delivery_channel_id: string;
  timezone: string;
  schedule_rules: ScheduleRules;
  stop_on_reply: boolean;
  stop_on_booking: boolean;
  steps?: CampaignStep[];
  frozen_at?: string | null;
  created_by?: string;
  created_at?: string;
  [key: string]: unknown;
}

export interface CampaignListItem {
  id: string;
  name: string;
  objective?: string | null;
  mail_account_id: string;
  owner_principal_id?: string;
  status: CampaignStatus | string;
  version: number;
  channel_ready?: boolean;
  blocking_reasons?: string[];
  created_at?: string;
  updated_at?: string;
  allowed_actions?: string[];
  [key: string]: unknown;
}

/** campaign_launches 행 — 승인되는 실제 실행 묶음 (§8.2). */
export interface CampaignLaunch {
  id: string;
  campaign_id?: string;
  revision_id: string;
  delivery_channel_id?: string;
  requested_by?: string;
  status: LaunchStatus | string;
  manifest_hash?: string | null; // sha256 hex
  frozen_at?: string | null;
  expires_at?: string;
  version: number;
  created_at?: string;
  recipients_count?: number;
  steps_count?: number;
  job_id?: string;
  execution_status?: CampaignLaunchExecutionStatus;
  activation_job_id?: string | null;
  allowed_actions?: string[];
  [key: string]: unknown;
}

/** GET /campaigns/{id} — 상태·revision·launch·성과 (B.2). */
export interface CampaignDetail extends CampaignListItem {
  revision?: CampaignRevision | null;
  revisions?: CampaignRevision[];
  launch?: CampaignLaunch | null;
  launches?: CampaignLaunch[];
  stats?: Record<string, number | string | null>;
}

/** POST /campaigns 요청 (B.3.6). */
export interface CreateCampaignInput {
  name: string;
  objective?: string;
  mail_account_id: string;
  delivery_channel_id: string;
  timezone: string;
  schedule_rules: ScheduleRules;
  steps: CampaignStepInput[];
  stop_on_reply: boolean;
  stop_on_booking: boolean;
}

/** POST /campaigns 응답 (B.3.6). */
export interface CreateCampaignResult {
  campaign_id: string;
  revision_id: string;
  status: string;
  revision_no?: number;
  version?: number;
  channel_ready?: boolean;
  blocking_reasons?: string[];
  [key: string]: unknown;
}

/** POST /campaigns/{id}/revisions — 변경된 구성·단계 → 새 revision. */
export type CreateRevisionInput = Omit<CreateCampaignInput, "name" | "objective" | "mail_account_id">;

export interface CreateRevisionResult {
  revision_id: string;
  revision_no?: number;
  status?: string;
  [key: string]: unknown;
}

/** POST /campaigns/{id}/preflight 요청·응답 (B.3.7). */
export interface PreflightInput {
  revision_id: string;
  email_address_ids: string[];
}

export interface PreflightRecipient {
  email_address_id: string;
  recipient_checks_passed: boolean;
  reason_codes: string[];
  [key: string]: unknown;
}

export interface PreflightResult {
  checked_at?: string;
  is_execution_authorization?: boolean;
  global_blocking_reasons?: string[];
  recipients: PreflightRecipient[];
  [key: string]: unknown;
}

/** POST /campaigns/{id}/launches — revision·대상·개인화 → 202 building/freeze job (B.3.8). */
export interface CreateLaunchInput {
  revision_id: string;
  email_address_ids: string[];
  tracking_requested?: boolean;
  personalization?: Record<string, Record<string, unknown>>;
}

export interface CreateLaunchResult {
  launch_id?: string;
  id?: string;
  status?: string;
  job_id?: string;
  version?: number;
  [key: string]: unknown;
}

/** `GET /campaign-launches/{id}` message item. */
export interface LaunchMessageSnapshot {
  outbound_message_id: string;
  enrollment_id: string;
  step_id: string;
  step_order: number;
  to_address_snapshot: string;
  subject: string;
  body_text: string;
  body_html: string | null;
  status: string;
  not_before: string | null;
  payload_hash: string;
}

export interface LaunchMessagePage {
  items: LaunchMessageSnapshot[];
  next_cursor: string | null;
  has_more: boolean;
}

export interface EnrollmentSummary {
  total: number;
  pending: number;
  active: number;
  completed: number;
  stopped: number;
  suppressed: number;
}

/** GET /campaign-launches/{id} — LaunchDetail wire DTO. */
export interface CampaignLaunchDetail extends CampaignLaunch {
  campaign_id?: string;
  revision_no?: number;
  mail_account_id?: string;
  step_count?: number;
  messages: LaunchMessagePage;
  enrollments: EnrollmentSummary;
  launch?: CampaignLaunch;
}

/** Normalize the flat DTO while tolerating a nested `launch` wrapper. */
export function normalizeLaunch(
  value: CampaignLaunchDetail | Record<string, unknown>
): CampaignLaunchDetail {
  const root = value as CampaignLaunchDetail;
  const nested = root.launch as CampaignLaunch | undefined;
  const rawMessages = root.messages as unknown;
  const page = Array.isArray(rawMessages)
    ? { items: rawMessages as LaunchMessageSnapshot[], next_cursor: null, has_more: false }
    : rawMessages && typeof rawMessages === "object"
      ? rawMessages as LaunchMessagePage
      : { items: [], next_cursor: null, has_more: false };
  const enrollmentSummary = root.enrollments && typeof root.enrollments === "object"
    ? root.enrollments
    : { total: 0, pending: 0, active: 0, completed: 0, stopped: 0, suppressed: 0 };
  return {
    ...nested,
    ...root,
    id: root.id ?? nested?.id ?? "",
    status: root.status ?? nested?.status ?? "unknown",
    manifest_hash: root.manifest_hash ?? nested?.manifest_hash,
    version: root.version ?? nested?.version ?? 1,
    messages: page,
    enrollments: enrollmentSummary as EnrollmentSummary,
  };
}

/** POST /campaign-launches/{id}/approval-requests (B.3.8). */
export interface LaunchApprovalRequestInput {
  expected_manifest_hash: string;
  reason?: string;
}

export interface LaunchApprovalRequestResult {
  approval_request_id: string;
  status?: string;
  expires_at?: string;
  version?: number;
  launch_id?: string;
  [key: string]: unknown;
}

export interface ExecuteCampaignLaunchInput {
  expected_manifest_hash: string;
  expected_version: number;
}

export interface ExecuteCampaignLaunchResult {
  launch_id: string;
  launch_status: "approved";
  execution_status: "queued";
  job_id: string;
  version: number;
  manifest_hash: string;
}

// ── 보조 리소스 (문서화 경로, 최소 타입) ─────────────────────────────

/** delivery_channels 행 (DDL) — 채널 게이트 표시용. */
export interface DeliveryChannel {
  id: string;
  mail_account_id: string;
  sender_identity_id?: string;
  provider: string;
  purpose: string;
  name: string;
  policy_status: "unverified" | "allowed" | "blocked" | string;
  enabled: boolean;
  capabilities?: Record<string, unknown>;
  internal_daily_cap?: number | null;
  version?: number;
  allowed_actions?: string[];
  [key: string]: unknown;
}

/** 대상 선택용 최소 담당자 타입 — 실제 행 형태는 관용적으로 수용한다. */
export interface AudienceContact {
  id: string;
  name?: string;
  display_name?: string;
  company_name?: string;
  company?: { name?: string } | null;
  emails?: AudienceEmail[];
  email_addresses?: AudienceEmail[];
  [key: string]: unknown;
}

export interface AudienceEmail {
  email_address_id?: string;
  id?: string;
  address?: string;
  email?: string;
  is_primary?: boolean;
  verification_status?: string;
  marketing_eligibility?: {
    eligible: boolean;
    reason_codes?: string[];
  } | null;
  [key: string]: unknown;
}

export interface ContactDetail extends AudienceContact {
  contacts_id?: string;
}

// ── 호출 ─────────────────────────────────────────────────────────────

export function getCampaign(id: string): Promise<ApiResult<CampaignDetail>> {
  return apiFetch<CampaignDetail>(
    `${CAMPAIGNS_PATH}/${encodeURIComponent(id)}`
  );
}

/** 캠페인 + 초기 revision 생성 (B.3.6) — 멱등 키 필수. */
export function createCampaign(
  input: CreateCampaignInput,
  options?: Pick<MutateOptions, "idempotencyKey">
): Promise<ApiResult<CreateCampaignResult>> {
  return mutate<CreateCampaignResult>(
    CAMPAIGNS_PATH,
    { method: "POST", json: input },
    { idempotencyKey: options?.idempotencyKey }
  );
}

/** 새 revision 생성 — 동일 approval 행 재사용 금지(A.6.2)에 따라 수정은 항상 새 revision. */
export function createRevision(
  campaignId: string,
  input: CreateRevisionInput,
  options?: Pick<MutateOptions, "idempotencyKey">
): Promise<ApiResult<CreateRevisionResult>> {
  return mutate<CreateRevisionResult>(
    `${CAMPAIGNS_PATH}/${encodeURIComponent(campaignId)}/revisions`,
    { method: "POST", json: input },
    { idempotencyKey: options?.idempotencyKey }
  );
}

/** 비구속 대상 검증 (B.3.7) — 부작용 없는 검사라 멱등 키 없이 호출한다. */
export function runPreflight(
  campaignId: string,
  input: PreflightInput
): Promise<ApiResult<PreflightResult>> {
  return apiFetch<PreflightResult>(
    `${CAMPAIGNS_PATH}/${encodeURIComponent(campaignId)}/preflight`,
    { method: "POST", json: input }
  );
}

/** 실행 묶음 생성·동결 요청 — 202 building + freeze job (B.3.8). 멱등 키 필수. */
export function createLaunch(
  campaignId: string,
  input: CreateLaunchInput,
  options?: Pick<MutateOptions, "idempotencyKey">
): Promise<ApiResult<CreateLaunchResult>> {
  return mutate<CreateLaunchResult>(
    `${CAMPAIGNS_PATH}/${encodeURIComponent(campaignId)}/launches`,
    { method: "POST", json: input },
    { idempotencyKey: options?.idempotencyKey }
  );
}

/** launch 상세 — cursor 를 넘기면 콘텐츠 다음 페이지를 조회한다 (B.3.8). */
export function getCampaignLaunch(
  launchId: string,
  cursor?: string | null
): Promise<ApiResult<CampaignLaunchDetail>> {
  const qs = cursor ? `?cursor=${encodeURIComponent(cursor)}` : "";
  return apiFetch<CampaignLaunchDetail>(
    `/api/v1/campaign-launches/${encodeURIComponent(launchId)}${qs}`
  );
}

/** 승인 요청 — expected hash·사유 (B.3.8). global block 상태면 422. */
export function requestLaunchApproval(
  launchId: string,
  input: LaunchApprovalRequestInput,
  options?: Pick<MutateOptions, "idempotencyKey">
): Promise<ApiResult<LaunchApprovalRequestResult>> {
  return mutate<LaunchApprovalRequestResult>(
    `/api/v1/campaign-launches/${encodeURIComponent(launchId)}/approval-requests`,
    { method: "POST", json: input },
    { idempotencyKey: options?.idempotencyKey }
  );
}

/** launch 일시정지 — 이유 + If-Match(버전) 필요 (B.2). */
export function executeCampaignLaunch(
  launchId: string,
  input: ExecuteCampaignLaunchInput,
  options: { idempotencyKey: string }
): Promise<ApiResult<ExecuteCampaignLaunchResult>> {
  return mutate<ExecuteCampaignLaunchResult>(
    `/api/v1/campaign-launches/${encodeURIComponent(launchId)}/execute`,
    { method: "POST", json: input },
    { idempotencyKey: options.idempotencyKey }
  );
}

export function pauseLaunch(
  launchId: string,
  input: { reason?: string },
  options?: MutateOptions
): Promise<ApiResult<CampaignLaunch>> {
  return mutate<CampaignLaunch>(
    `/api/v1/campaign-launches/${encodeURIComponent(launchId)}/pause`,
    { method: "POST", json: input },
    { etag: options?.etag, idempotencyKey: options?.idempotencyKey }
  );
}

/** 미실행 취소 — in-flight 건은 별도 표시 (B.2, §8.6). */
export function cancelLaunch(
  launchId: string,
  input: { reason?: string },
  options?: MutateOptions
): Promise<ApiResult<CampaignLaunch>> {
  return mutate<CampaignLaunch>(
    `/api/v1/campaign-launches/${encodeURIComponent(launchId)}/cancel`,
    { method: "POST", json: input },
    { etag: options?.etag, idempotencyKey: options?.idempotencyKey }
  );
}

export function getContact(id: string): Promise<ApiResult<ContactDetail>> {
  return apiFetch<ContactDetail>(
    `${CONTACTS_PATH}/${encodeURIComponent(id)}`
  );
}

// ── 응답 정규화 헬퍼 ─────────────────────────────────────────────────

/** 담당자 상세/목록에서 주소 배열을 관용적으로 추출한다. */
export function contactEmails(contact: AudienceContact | ContactDetail | null | undefined): AudienceEmail[] {
  if (!contact) return [];
  const raw = contact.emails ?? contact.email_addresses;
  return Array.isArray(raw) ? raw : [];
}

export function audienceEmailId(email: AudienceEmail): string | undefined {
  return email.email_address_id ?? email.id;
}

export function audienceEmailAddress(email: AudienceEmail): string | undefined {
  return email.address ?? email.email;
}
