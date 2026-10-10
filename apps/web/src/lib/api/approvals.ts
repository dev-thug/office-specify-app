/**
 * approvals API — 소스 §5.5, §8.2, A.6.3, B.2(승인 rows), B.3.9.
 * openapi.json 생성 전 손 타이핑 — 필드명은 부록 B wire 형식 그대로.
 */
import { apiFetch, type ApiResult } from "./client";
import { mutate, type MutateOptions } from "./mutate";

export const APPROVALS_PATH = "/api/v1/approvals";

// ── 상태 집합 (DDL CHECK) ────────────────────────────────────────────

export type ApprovalActionType =
  | "campaign_launch"
  | "message_submit"
  | "booking_change"
  | "suppression_release";

export type ApprovalStatus =
  | "pending"
  | "approved"
  | "rejected"
  | "expired"
  | "revoked";

export type ApprovalDecisionValue = "approved" | "rejected";

// ── DTO ──────────────────────────────────────────────────────────────

export interface ApprovalListItem {
  id: string;
  action_type: ApprovalActionType | string;
  status: ApprovalStatus | string;
  requested_by: string;
  target_id?: string;
  requester_display_name?: string;
  requester?: { id?: string; name?: string; display_name?: string; kind?: string } | null;
  /** 구체적 대상 FK — action_type 에 따라 하나만 채워진다 (DDL CHECK). */
  launch_id?: string | null;
  outbound_message_id?: string | null;
  booking_change_id?: string | null;
  suppression_id?: string | null;
  snapshot_hash?: string;
  expires_at?: string;
  created_at?: string;
  version: number;
  allowed_actions?: string[];
  [key: string]: unknown;
}

/** 승인 화면에 보이는 정책 검사 결과 한 줄. */
export interface ApprovalPolicyResult {
  code?: string;
  status?: string;
  outcome?: string; // pass | warn | fail 등 — 서버 문자열 그대로
  detail?: string;
  [key: string]: unknown;
}

/** 동결 스냅샷 요약 — 서버 계약 확정 전까지 관용적으로 수용한다. */
export interface ApprovalSnapshotMessage {
  to?: string;
  to_address_snapshot?: string;
  email_address_id?: string;
  step_order?: number;
  subject?: string;
  body_text?: string;
  body_html?: string | null;
  [key: string]: unknown;
}

export interface ApprovalSnapshot {
  recipients_count?: number;
  sender_address?: string;
  sender?: string;
  subject?: string;
  body_text?: string;
  body_html?: string | null;
  purpose?: string;
  messages?: ApprovalSnapshotMessage[];
  items?: ApprovalSnapshotMessage[];
  enrollments?: { total?: number; [key: string]: unknown };
  [key: string]: unknown;
}

export interface ApprovalTargetSummary {
  status?: string | null;
  current_hash?: string | null;
  mail_account_id?: string | null;
  preview?: ApprovalSnapshot | null;
}

export interface ApprovalDecision {
  id?: string;
  decision: ApprovalDecisionValue | string;
  decided_by?: string;
  decided_at?: string;
  reason?: string | null;
  version_at_decision?: number;
  [key: string]: unknown;
}

export interface ApprovalNativeExport {
  id: string;
  export_hash: string;
  state: string;
  subject: string;
  body_html: string;
  field_map: Record<
    string,
    { contact_field: string; merge_tag: string }
  >;
  recipient_count: number;
  sender_address: string;
  provider_list_key: string | null;
  topic_id: string | null;
}

/** GET /approvals/{id} — 동결 스냅샷·policy·권한 (B.2). */
export interface ApprovalRequestDetail extends ApprovalListItem {
  target?: ApprovalTargetSummary;
  decision?: ApprovalDecision | null;
  snapshot?: ApprovalSnapshot | null;
  policy_results?: ApprovalPolicyResult[];
  /** 서버가 내려주는 보조 힌트 — 있으면 버튼 게이팅에 우선 사용한다. */
  self_request?: boolean;
  can_decide?: boolean;
  recipients_count?: number;
  manifest_hash?: string;
  /** True when approval must bind to the transformed Zoho Campaigns export. */
  native_export_required?: boolean;
  native_export?: ApprovalNativeExport | null;
}

/** POST /approvals/{id}/decisions 요청 (B.3.9). */
export interface ApprovalDecisionInput {
  decision: ApprovalDecisionValue;
  expected_snapshot_hash: string;
  expected_export_hash?: string;
  reason?: string;
}

/** B.3.9 응답 — 200 OK + 실행 상태(비동기). 발송 완료가 아니다. */
export interface ApprovalDecisionResult {
  approval_request_id?: string;
  approval_status?: string;
  decision_id?: string;
  execution_status?: string;
  job_id?: string;
  provider_accepted_count?: number;
  version?: number;
  [key: string]: unknown;
}

// ── 호출 ─────────────────────────────────────────────────────────────

export function getApproval(
  id: string
): Promise<ApiResult<ApprovalRequestDetail>> {
  return apiFetch<ApprovalRequestDetail>(
    `${APPROVALS_PATH}/${encodeURIComponent(id)}`
  );
}

/**
 * 승인 결정 — If-Match(버전) + Idempotency-Key 필수 (B.3.9).
 * expected_snapshot_hash 는 검토한 동결 스냅샷과 일치해야 한다(SNAPSHOT_MISMATCH).
 */
export function postApprovalDecision(
  id: string,
  input: ApprovalDecisionInput,
  options?: MutateOptions
): Promise<ApiResult<ApprovalDecisionResult>> {
  return mutate<ApprovalDecisionResult>(
    `${APPROVALS_PATH}/${encodeURIComponent(id)}/decisions`,
    { method: "POST", json: input },
    { etag: options?.etag, idempotencyKey: options?.idempotencyKey }
  );
}

/** 아직 미실행인 허가 철회 — 승인자·Admin (B.2). */
export function revokeApproval(
  id: string,
  input: { reason?: string },
  options?: MutateOptions
): Promise<ApiResult<ApprovalListItem>> {
  return mutate<ApprovalListItem>(
    `${APPROVALS_PATH}/${encodeURIComponent(id)}/revoke`,
    { method: "POST", json: input },
    { etag: options?.etag, idempotencyKey: options?.idempotencyKey }
  );
}
