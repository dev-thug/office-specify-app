/**
 * 준수(수신 거부·발송 근거) API 래퍼 — 기능 작업자 소유.
 *
 * 계약 출처: 부록 B.2 "예약·준수·운영" 표(3627–3631행)와 B.4.4 공개 수신 거부.
 * openapi.json 생성 전이라 DDL CHECK 값(부록 A)과 B.2 요약으로 손으로 타이핑한다.
 * 스키마가 생성되면 호출부를 유지한 채 paths 타입으로 갈아끼운다 (DESIGN.md).
 *
 * B.2가 요청 본문을 필드 단위로 적지 않아 아래를 가정했다 (구현 후 계약 diff 확인):
 * - POST /suppressions: { address, purpose_scope, reason, evidence_ref?, source_outbound_id? }
 *   — "주소·목적·이유 → 차단". 서버가 address를 정규화해 email_addresses에 연결한다고 가정.
 * - POST /communication-bases: { address, purpose, jurisdiction, basis_type,
 *   evidence_ref, collected_at, valid_until? } — "신규는 pending".
 * - reviews 본문: { decision: "valid"|"rejected", valid_until?, reason? }.
 * - revoke 본문: { reason }.
 * - release-requests 본문: { reason, evidence_ref } → 승인 요청(직접 해제 아님).
 * - GET /public/v1/unsubscribe/{token}: 발신 조직 표시명·범위·마스킹 주소·상태
 *   (§7.6 "발신 조직과 차단 대상 범위", 주소 전체·CRM ID 노출 금지).
 */
import { apiFetch, type ApiResult } from "./client";
import { mutate, type MutateOptions } from "./mutate";
import type { ListParams } from "./use-cursor-list";

// ── enum — DDL CHECK 값 그대로 ────────────────────────────────────

/** suppressions.purpose_scope — unsubscribe 토큰은 항상 marketing (DDL 1781). */
export type SuppressionPurposeScope = "marketing" | "all";
export type SuppressionReason =
  | "unsubscribe"
  | "complaint"
  | "hard_bounce"
  | "manual";

/** communication_bases.purpose — B.1.1 Purpose 집합. */
export type BasisPurpose = "marketing" | "correspondence" | "transactional";
export type BasisType =
  | "explicit_consent"
  | "requested_reply"
  | "requested_transaction"
  | "reviewed_exception";
export type BasisStatus =
  | "pending"
  | "valid"
  | "rejected"
  | "revoked"
  | "expired";

// ── suppressions ──────────────────────────────────────────────────

export const SUPPRESSIONS_PATH = "/api/v1/suppressions";

/** suppressions 행 — DDL 1743–1758 + 목록 표시용 주소(서버 join 가정, 옵션). */
export interface Suppression {
  id: string;
  email_address_id: string;
  /** 표시용 주소 — B.2에 명시가 없어 없으면 id로 대체 표시한다. */
  email_address?: string;
  purpose_scope: SuppressionPurposeScope;
  reason: SuppressionReason;
  source_outbound_id: string | null;
  created_by: string | null;
  released_at: string | null;
  released_by: string | null;
  release_evidence_ref: string | null;
  version: number;
  created_at: string;
  /** 화면 힌트 — 서버 재검사를 대신하지 않는다 (B.1.1 AllowedActions). */
  allowed_actions?: string[];
  [key: string]: unknown;
}

/**
 * GET /suppressions 의 "제한된 검색" 파라미터 — B.2가 필드 allowlist까지만
 * 문서화해 아래 이름을 가정한다: q(주소 검색)·reason·purpose_scope·released.
 */
export interface SuppressionListParams extends ListParams {
  q?: string;
  reason?: SuppressionReason;
  purpose_scope?: SuppressionPurposeScope;
  /** true: 해제된 행만 / false: 차단 중인 행만 / 생략: 전체 */
  released?: boolean;
}

export interface CreateSuppressionInput {
  /** 정규화 전 원문 주소 — 서버가 normalize해 email_addresses에 연결한다고 가정. */
  address: string;
  purpose_scope: SuppressionPurposeScope;
  reason: SuppressionReason;
  /** 수동 등록·외부 수신 거부의 근거 참조 — 있을 때만 전송한다. */
  evidence_ref?: string;
  /** complaint/hard_bounce가 특정 발송에서 비롯됐을 때의 출처. */
  source_outbound_id?: string;
}

/**
 * 차단 추가 (POST /api/v1/suppressions).
 * 멱등 생성이라 If-Match 없이 Idempotency-Key만 싣는다 (B.1.4 두 번째 문단).
 */
export function createSuppression(
  input: CreateSuppressionInput,
  options?: MutateOptions
): Promise<ApiResult<Suppression>> {
  return mutate<Suppression>(
    SUPPRESSIONS_PATH,
    { method: "POST", json: input },
    options
  );
}

/**
 * 해제 요청 응답 — 승인 요청 리소스를 가리키는 최소 참조.
 * B.2: "증빙·이유 → 승인 요청; 직접 해제 아님". approval_requests 행 또는 그
 * id를 돌려준다고 가정하고 느슨하게 타이핑한다.
 */
export interface SuppressionReleaseRequestResult {
  approval_request_id?: string;
  id?: string;
  status?: string;
  [key: string]: unknown;
}

export interface CreateReleaseRequestInput {
  /** 사람이 요청하는 사유 — 감사 메타데이터에 남는다. */
  reason: string;
  /** 해제 근거 증빙 — DDL CHECK가 실제 release 시 release_evidence_ref를 요구. */
  evidence_ref: string;
}

/** POST /suppressions/{id}/release-requests — 직접 해제가 아니라 승인 요청 생성. */
export function requestSuppressionRelease(
  id: string,
  input: CreateReleaseRequestInput,
  options?: MutateOptions
): Promise<ApiResult<SuppressionReleaseRequestResult>> {
  return mutate<SuppressionReleaseRequestResult>(
    `${SUPPRESSIONS_PATH}/${encodeURIComponent(id)}/release-requests`,
    { method: "POST", json: input },
    options
  );
}

// ── communication-bases ───────────────────────────────────────────

export const COMMUNICATION_BASES_PATH = "/api/v1/communication-bases";

/** communication_bases 행 — DDL 1361–1380 + 목록 표시용 주소(옵션). */
export interface CommunicationBasis {
  id: string;
  email_address_id: string;
  /** 표시용 주소 — 없으면 id로 대체 표시한다. */
  email_address?: string;
  purpose: BasisPurpose;
  jurisdiction: string;
  basis_type: BasisType;
  status: BasisStatus;
  evidence_ref: string;
  collected_at: string;
  reviewed_by: string | null;
  reviewed_at: string | null;
  valid_until: string | null;
  revoked_at: string | null;
  version: number;
  created_at: string;
  allowed_actions?: string[];
  [key: string]: unknown;
}

export interface CommunicationBasisListParams extends ListParams {
  q?: string;
  status?: BasisStatus;
  purpose?: BasisPurpose;
}

export interface CreateCommunicationBasisInput {
  /** 정규화 전 원문 주소 — suppressions와 같은 가정. */
  address: string;
  purpose: BasisPurpose;
  jurisdiction: string;
  basis_type: BasisType;
  evidence_ref: string;
  /** RFC 3339 — 근거가 실제로 수집된 시각. */
  collected_at: string;
  valid_until?: string;
}

/**
 * 근거 등록 (POST /communication-bases) — 신규는 항상 pending이며
 * 사람 검토 없이 valid가 되지 않는다 (§11.4, DDL CHECK 1378).
 */
export function createCommunicationBasis(
  input: CreateCommunicationBasisInput,
  options?: MutateOptions
): Promise<ApiResult<CommunicationBasis>> {
  return mutate<CommunicationBasis>(
    COMMUNICATION_BASES_PATH,
    { method: "POST", json: input },
    options
  );
}

export type BasisReviewDecision = "valid" | "rejected";

export interface ReviewCommunicationBasisInput {
  decision: BasisReviewDecision;
  /** decision=valid 일 때 유효 기간 (RFC 3339). */
  valid_until?: string;
  /** 검토 메모·거절 사유. */
  reason?: string;
}

/**
 * 근거 검토 (POST /communication-bases/{id}/reviews) — 인간 검토자 전용.
 * 버전 관리 엔티티의 결정이라 If-Match 를 싣는다 (B.1.4 승인 결정 준용).
 */
export function reviewCommunicationBasis(
  id: string,
  input: ReviewCommunicationBasisInput,
  options?: MutateOptions
): Promise<ApiResult<CommunicationBasis>> {
  return mutate<CommunicationBasis>(
    `${COMMUNICATION_BASES_PATH}/${encodeURIComponent(id)}/reviews`,
    { method: "POST", json: input },
    options
  );
}

export interface RevokeCommunicationBasisInput {
  /** 철회 사유 — 관련 발송 중단의 감사 근거가 된다. */
  reason: string;
}

/** POST /communication-bases/{id}/revoke — 철회 + 관련 발송 중단. */
export function revokeCommunicationBasis(
  id: string,
  input: RevokeCommunicationBasisInput,
  options?: MutateOptions
): Promise<ApiResult<CommunicationBasis>> {
  return mutate<CommunicationBasis>(
    `${COMMUNICATION_BASES_PATH}/${encodeURIComponent(id)}/revoke`,
    { method: "POST", json: input },
    options
  );
}

// ── 공개 수신 거부 (B.4.4) ────────────────────────────────────────

/**
 * GET /public/v1/unsubscribe/{token} 확인 데이터 — §7.6 "발신 조직과 차단 대상
 * 범위". 주소 전체·CRM ID는 노출되지 않으며 마스킹 형태만 올 수 있다.
 * 응답 필드명은 문서에 없어 모두 옵션으로 둔다.
 */
export interface UnsubscribePreview {
  /** 발신 조직의 공개 표시명. */
  organization_name?: string;
  /** 마스킹된 대상 주소 (예: "j***@example.com"). */
  masked_address?: string;
  /** 차단 범위 — POST 응답과 같은 문자열 ("workspace_marketing"). */
  scope?: string;
  /** 토큰 상태 — 이미 처리/만료된 링크를 상태로 알릴 때 사용한다고 가정. */
  status?: string;
  [key: string]: unknown;
}

/** POST 응답 — B.4.4 예시 {status:"unsubscribed", scope, message}. */
export interface UnsubscribeResult {
  status: string;
  scope?: string;
  message?: string;
  [key: string]: unknown;
}

/**
 * 수신 거부 확인 데이터 — GET은 상태를 바꾸지 않는다 (§11.2).
 * 알 수 없는 토큰은 일반화된 404로 와야 한다 (주소 존재 은닉).
 */
export function getUnsubscribePreview(
  token: string
): Promise<ApiResult<UnsubscribePreview>> {
  return apiFetch<UnsubscribePreview>(
    `/public/v1/unsubscribe/${encodeURIComponent(token)}`
  );
}

/**
 * 수신 거부 실행 — 본문은 빈 객체 (B.4.4). 반복 요청은 같은 성공 응답이며,
 * 공개 capability라 세션 CSRF가 필요 없다 (client가 쿠키가 있을 때만 자동 부가).
 */
export function confirmUnsubscribe(
  token: string,
  options?: MutateOptions
): Promise<ApiResult<UnsubscribeResult>> {
  return mutate<UnsubscribeResult>(
    `/public/v1/unsubscribe/${encodeURIComponent(token)}`,
    { method: "POST", json: {} },
    options
  );
}
