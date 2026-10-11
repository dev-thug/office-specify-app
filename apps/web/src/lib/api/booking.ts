/**
 * 공개 예약 API — 소스 B.4 (4065–4138) 기준 손 타이핑.
 * 내부 세션과 무관하게 토큰 capability 만으로 동작한다 — 세션 CSRF 쿠키가 없으면
 * client.ts가 헤더를 싣지 않으므로 그대로 둔다.
 *
 *   GET  /public/v1/booking-links/{token}             — 공개 미팅 설명·호스트·제한 규칙
 *   GET  /public/v1/booking-links/{token}/slots       — ?from&to&timezone 슬롯
 *   POST /public/v1/booking-links/{token}/bookings    — 예약 요청 → 202 confirming
 *   GET  /public/v1/bookings/{manage_token}           — 진행·관리 데이터
 *   POST /public/v1/bookings/{manage_token}/reschedule — 새 슬롯 + expected version → 202
 *   POST /public/v1/bookings/{manage_token}/cancel     — 취소 + expected version → 202
 *
 * 공개 변경은 If-Match 헤더가 아니라 본문 expected_version 을 쓴다 — B.4.1 표기가
 * "새 슬롯·expected version" 이기 때문이다 (보고서 가정).
 */
import { apiFetch, idempotencyKey, type ApiResult } from "./client";
import { mutate } from "./mutate";

// ── 예약 링크 (초대 페이지) ──────────────────────────────────

/** GET booking-links/{token} data — "공개 미팅 설명·호스트 이름·제한된 규칙". */
export interface PublicBookingLink {
  meeting_type_id?: string;
  /** 공개 제목 — 미팅 유형 이름. */
  name?: string;
  title?: string;
  description?: string | null;
  /** 호스트 공개 이름 — 내부 일정 내용은 노출되지 않는다 (§7.6). */
  host_display_name?: string;
  host_name?: string;
  duration_minutes?: number;
  /** 링크/호스트 기준 표시 시간대 힌트 — 게스트는 자기 시간대로 바꿔 본다. */
  timezone?: string;
  minimum_notice_minutes?: number;
  booking_horizon_days?: number;
  /** 예약 요청 시 그대로 에코해야 하는 고지 버전. */
  privacy_notice_version: string;
  /** 미팅 방식과 장소 안내 — 오프라인 주소는 예약 전에도 보인다. */
  location_kind?: "online" | "offline";
  location_detail?: string | null;
  /** 특정 수신자에게 발급된 링크(아웃리치)면 그 수신자 — 폼을 미리 채운다. */
  invitee?: PublicBookingInvitee;
  [key: string]: unknown;
}

export interface PublicBookingInvitee {
  email: string;
  name?: string | null;
  company_name?: string | null;
}

/** GET slots data — B.4.2 예시 그대로. */
export interface PublicSlot {
  starts_at: string;
  ends_at: string;
}

export interface PublicSlotsData {
  timezone: string;
  availability_checked_at?: string;
  slots: PublicSlot[];
  /** 슬롯은 조회 시점 후보일 뿐 — true 라고 단정해 표시하지 않는다. */
  reservation_guaranteed?: boolean;
  [key: string]: unknown;
}

export function getPublicBookingLink(
  token: string
): Promise<ApiResult<PublicBookingLink>> {
  return apiFetch<PublicBookingLink>(
    `/public/v1/booking-links/${encodeURIComponent(token)}`
  );
}

/** 이미 예약에 쓰인 초대 링크 → 그 예약의 관리 페이지 (수신자 지정·1회용 링크만). */
export function getPublicLinkBooking(
  token: string
): Promise<ApiResult<{ manage_url: string }>> {
  return apiFetch<{ manage_url: string }>(
    `/public/v1/booking-links/${encodeURIComponent(token)}/booking`
  );
}

export interface PublicSlotsQuery {
  /** YYYY-MM-DD — timezone 기준 로컬 날짜 (B.4.2). */
  from: string;
  to: string;
  timezone: string;
}

export function getPublicSlots(
  token: string,
  query: PublicSlotsQuery
): Promise<ApiResult<PublicSlotsData>> {
  const qs = new URLSearchParams({
    from: query.from,
    to: query.to,
    timezone: query.timezone,
  });
  return apiFetch<PublicSlotsData>(
    `/public/v1/booking-links/${encodeURIComponent(token)}/slots?${qs}`
  );
}

// ── 예약 생성 → 202 confirming ───────────────────────────────

export interface PublicBookingCreateInput {
  guest_name: string;
  guest_email: string;
  /** 선택한 슬롯의 시작 시각 (UTC). */
  starts_at: string;
  /** 게스트가 슬롯을 본 시간대 — DST 판단의 근거로 서버에 남긴다. */
  timezone: string;
  /** Optional guest note (B.4 extension; bounded to 2,000 characters). */
  memo?: string;
  /** 링크 응답이 준 고지 버전 — 없으면 생략. */
  privacy_notice_version: string;
}

/** POST bookings 응답 data — B.4.3 예시. */
export interface PublicBookingAccepted {
  status: string;
  starts_at?: string;
  ends_at?: string;
  timezone?: string;
  manage_url?: string;
  status_url?: string;
  message?: string;
  [key: string]: unknown;
}

/**
 * 예약 요청 — Idempotency-Key 필수(B.1.3). 네트워크 재시도는 같은 키를 써야 하므로
 * 호출자가 useRef 로 보유한 키를 넘긴다.
 */
export function createPublicBooking(
  token: string,
  input: PublicBookingCreateInput,
  options?: { idempotencyKey?: string }
): Promise<ApiResult<PublicBookingAccepted>> {
  return mutate<PublicBookingAccepted>(
    `/public/v1/booking-links/${encodeURIComponent(token)}/bookings`,
    { method: "POST", json: input },
    { idempotencyKey: options?.idempotencyKey ?? idempotencyKey() }
  );
}

// ── 예약 관리 (manage 토큰) ──────────────────────────────────

/** GET bookings/{manage_token} data — 진행 상태·세부·버전. */
export interface PublicManagedBooking {
  id?: string;
  status: string;
  starts_at?: string;
  ends_at?: string;
  timezone?: string;
  guest_timezone?: string;
  guest_name?: string;
  meeting_name?: string | null;
  meeting_description?: string | null;
  duration_minutes?: number;
  booking_horizon_days?: number;
  location_kind?: "online" | "offline";
  location_detail?: string | null;
  host_display_name?: string | null;
  failure_code?: string | null;
  /** expected_version 으로 되돌려 보낼 현재 버전. */
  version?: number;
  message?: string;
  [key: string]: unknown;
}

export function getManagedBooking(
  manageToken: string
): Promise<ApiResult<PublicManagedBooking>> {
  return apiFetch<PublicManagedBooking>(
    `/public/v1/bookings/${encodeURIComponent(manageToken)}`
  );
}

/** 시간 변경 후보 — 처음 초대 링크와 같은 가용 시간 (GET bookings/{manage_token}/slots). */
export function getManagedSlots(
  manageToken: string,
  query: PublicSlotsQuery
): Promise<ApiResult<PublicSlotsData>> {
  const qs = new URLSearchParams({ from: query.from, to: query.to, timezone: query.timezone });
  return apiFetch<PublicSlotsData>(
    `/public/v1/bookings/${encodeURIComponent(manageToken)}/slots?${qs}`
  );
}

export interface PublicRescheduleInput {
  starts_at: string;
  timezone: string;
  /** GET 응답의 현재 version — 오래된 변경 요청은 412. */
  expected_version: number;
}

export function rescheduleManagedBooking(
  manageToken: string,
  input: PublicRescheduleInput,
  options?: { idempotencyKey?: string }
): Promise<ApiResult<PublicManagedBooking>> {
  return mutate<PublicManagedBooking>(
    `/public/v1/bookings/${encodeURIComponent(manageToken)}/reschedule`,
    { method: "POST", json: input },
    { idempotencyKey: options?.idempotencyKey ?? idempotencyKey() }
  );
}

export interface PublicCancelInput {
  expected_version: number;
}

export function cancelManagedBooking(
  manageToken: string,
  input: PublicCancelInput,
  options?: { idempotencyKey?: string }
): Promise<ApiResult<PublicManagedBooking>> {
  return mutate<PublicManagedBooking>(
    `/public/v1/bookings/${encodeURIComponent(manageToken)}/cancel`,
    { method: "POST", json: input },
    { idempotencyKey: options?.idempotencyKey ?? idempotencyKey() }
  );
}
