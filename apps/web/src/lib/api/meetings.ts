/**
 * meetings / meeting-types 내부 API — 소스 부록 B.2 "예약·준수·운영" 표 기준 손 타이핑.
 * openapi 스키마 생성 후에도 이 래퍼를 유지해 호출부가 경로 문자열에 의존하지 않게 한다.
 *
 * 엔드포인트 (B.2 3618–3626):
 *   GET/POST /api/v1/meeting-types            — 유형 목록 / 생성
 *   PATCH    /api/v1/meeting-types/{id}       — 가용 설정 변경 (If-Match)
 *   GET/PUT  /api/v1/hosts/{id}/availability  — 호스트 가용 규칙·예외
 *   POST     /api/v1/booking-links            — 예약 링크 발급 (토큰 일회성 노출)
 *   GET      /api/v1/bookings                 — 기간·상태·host 필터 목록
 *   GET      /api/v1/bookings/{id}            — 예약 상세·외부 sync·CRM
 *   POST     /api/v1/bookings/{id}/reschedule — 새 슬롯 + If-Match → 202
 *   POST     /api/v1/bookings/{id}/cancel     — 취소 요청 + If-Match → 202
 *
 * 보조 조회(다른 영역 소유 endpoint를 읽기만 한다 — 파일은 이 모듈 소유):
 *   GET /api/v1/principals (조직 조회) — 호스트 이름·필터용
 *   GET /api/v1/calendar-connections  — 미팅 유형의 캘린더 연결 선택용
 *
 * 참석 결과 저장 경로는 B.2에 없으며 booking DTO도 이를 반환하지 않는다.
 */
import { apiFetch, type ApiResult } from "./client";
import { mutate } from "./mutate";

// ── 미팅 유형 ────────────────────────────────────────────────

/** meeting_types.status CHECK 집합 (DDL). */
export type MeetingTypeStatus = "active" | "disabled";

export interface MeetingType {
  id: string;
  host_principal_id: string;
  calendar_connection_id: string;
  name: string;
  description?: string | null;
  duration_minutes: number;
  buffer_before_minutes: number;
  buffer_after_minutes: number;
  minimum_notice_minutes: number;
  booking_horizon_days: number;
  location_kind?: MeetingLocationKind;
  location_detail?: string | null;
  status: MeetingTypeStatus | string;
  version?: number;
  created_at?: string;
  allowed_actions?: string[];
  [key: string]: unknown;
}

/** 생성 요청 — host 교체는 신규 생성만 허용된다(B.2). */
export interface MeetingTypeCreateInput {
  calendar_connection_id: string;
  name: string;
  description?: string;
  duration_minutes: number;
  buffer_before_minutes: number;
  buffer_after_minutes: number;
  minimum_notice_minutes: number;
  booking_horizon_days: number;
  location_kind: MeetingLocationKind;
  location_detail?: string;
}

/** PATCH 허용 필드 — host 교체는 불가(B.2 "host 교체는 신규 생성"). */
export interface MeetingTypePatchInput {
  name?: string;
  description?: string | null;
  duration_minutes?: number;
  buffer_before_minutes?: number;
  buffer_after_minutes?: number;
  minimum_notice_minutes?: number;
  booking_horizon_days?: number;
  location_kind?: MeetingLocationKind;
  location_detail?: string | null;
  status?: MeetingTypeStatus;
}

/** 미팅 방식 — 온라인(화상) / 오프라인(대면). */
export type MeetingLocationKind = "online" | "offline";

export const MEETING_TYPES_PATH = "/api/v1/meeting-types";
export const MEETING_TYPES_QUERY_KEY = ["meetings", "types"] as const;
export const MEETING_TYPES_LIST_LIMIT = 200;

export function createMeetingType(
  input: MeetingTypeCreateInput,
  options?: { idempotencyKey?: string }
): Promise<ApiResult<MeetingType>> {
  return mutate<MeetingType>(MEETING_TYPES_PATH, {
    method: "POST",
    json: input,
  }, { idempotencyKey: options?.idempotencyKey });
}

export function updateMeetingType(
  id: string,
  patch: MeetingTypePatchInput,
  options: { etag: string | number; idempotencyKey?: string }
): Promise<ApiResult<MeetingType>> {
  return mutate<MeetingType>(
    `${MEETING_TYPES_PATH}/${encodeURIComponent(id)}`,
    { method: "PATCH", json: patch },
    { etag: options.etag, idempotencyKey: options.idempotencyKey }
  );
}

// ── 호스트 가용 규칙·예외 ────────────────────────────────────

/** availability_rules 행 — weekday 1(월)~7(일), 로컬 시각은 "HH:MM". */
export interface AvailabilityRule {
  id?: string;
  weekday: number;
  start_local: string;
  end_local: string;
  timezone: string;
  valid_from?: string | null;
  valid_until?: string | null;
  [key: string]: unknown;
}

/** availability_exceptions 행 — kind CHECK ('blocked','available'). */
export type AvailabilityExceptionKind = "blocked" | "available";

export interface AvailabilityException {
  id?: string;
  starts_at: string;
  ends_at: string;
  kind: AvailabilityExceptionKind | string;
  reason?: string | null;
  [key: string]: unknown;
}

/** GET /hosts/{id}/availability 응답 data — 전체 교체 PUT 의 페이로드와 동형. */
export interface HostAvailability {
  rules: AvailabilityRule[];
  exceptions: AvailabilityException[];
  /** 응답에 포함되면 If-Match 대체 값으로 쓴다 (ETag 헤더 우선). */
  version?: number;
  [key: string]: unknown;
}

export interface HostAvailabilityPutInput {
  rules: AvailabilityRule[];
  exceptions: AvailabilityException[];
}

export function hostAvailabilityPath(hostId: string): string {
  return `/api/v1/hosts/${encodeURIComponent(hostId)}/availability`;
}

export function getHostAvailability(
  hostId: string
): Promise<ApiResult<HostAvailability>> {
  return apiFetch<HostAvailability>(hostAvailabilityPath(hostId));
}

/**
 * 가용 규칙·예외 전체 교체. 서버가 ETag/"version"을 주면 If-Match로 보낸다 —
 * 없으면 헤더를 생략하고 서버의 428을 그대로 표면화한다.
 */
export function putHostAvailability(
  hostId: string,
  input: HostAvailabilityPutInput,
  options?: { etag?: string | number; idempotencyKey?: string }
): Promise<ApiResult<HostAvailability>> {
  return mutate<HostAvailability>(
    hostAvailabilityPath(hostId),
    { method: "PUT", json: input },
    { etag: options?.etag, idempotencyKey: options?.idempotencyKey }
  );
}

// ── 예약 링크 ────────────────────────────────────────────────

export interface BookingLinkCreateInput {
  meeting_type_id: string;
  /** 초대 대상 주소 바인딩이 필요할 때 (선택). */
  bound_email_address_id?: string;
  max_uses?: number;
  expires_at?: string;
}

/** 생성 응답 — 원문 토큰·URL은 이 응답에서만 보인다. */
export interface BookingLink {
  id: string;
  meeting_type_id?: string;
  url?: string;
  booking_url?: string;
  token_url?: string;
  token?: string;
  max_uses?: number;
  expires_at?: string | null;
  created_at?: string;
  [key: string]: unknown;
}

export function createBookingLink(
  input: BookingLinkCreateInput,
  options?: { idempotencyKey?: string }
): Promise<ApiResult<BookingLink>> {
  return mutate<BookingLink>(
    "/api/v1/booking-links",
    { method: "POST", json: input },
    { idempotencyKey: options?.idempotencyKey }
  );
}

// ── 예약(미팅) ───────────────────────────────────────────────

/** bookings.status CHECK 집합 (DDL 1889). */
export type BookingStatus =
  | "held"
  | "confirming"
  | "confirmed"
  | "canceling"
  | "canceled"
  | "failed"
  | "unknown"
  | "expired";

/** 게스트 이메일 소유 확인 상태 (DDL CHECK). */
export type GuestEmailVerification =
  | "unverified"
  | "invitation_bound"
  | "verified";

export interface Booking {
  id: string;
  booking_link_id?: string;
  host_principal_id: string;
  guest_email_address_id?: string;
  guest_name: string;
  guest_email?: string | null;
  guest_timezone?: string;
  guest_email_verification_status?: GuestEmailVerification | string;
  meeting_type_id?: string;
  meeting_type_name?: string | null;
  host_display_name?: string | null;
  starts_at: string;
  ends_at: string;
  status: BookingStatus | string;
  hold_expires_at?: string | null;
  provider_event_id?: string | null;
  provider_revision?: string | null;
  confirmation_verified_at?: string | null;
  failure_code?: string | null;
  attendance_status?: "attended" | "no_show" | string | null;
  outcome_note?: string | null;
  version?: number;
  created_at?: string;
  allowed_actions?: string[];
  [key: string]: unknown;
}

export const BOOKINGS_PATH = "/api/v1/bookings";

/** GET /bookings 필터 — B.2 "기간·상태·host 목록". */
export interface BookingListParams {
  from?: string;
  to?: string;
  status?: BookingStatus | string;
  host_principal_id?: string;
}

export function getBooking(id: string): Promise<ApiResult<Booking>> {
  return apiFetch<Booking>(`${BOOKINGS_PATH}/${encodeURIComponent(id)}`);
}

export interface RescheduleBookingInput {
  /** 새 슬롯 시작 시각 (RFC 3339 UTC). */
  starts_at: string;
  /** 슬롯을 알고 있을 때만 함께 보낸다 — 서버는 유형의 duration을 신뢰해도 된다. */
  ends_at?: string;
}

/** 일정 변경 — 202 비동기 saga. If-Match 필수 (B.1.4). */
export function rescheduleBooking(
  id: string,
  input: RescheduleBookingInput,
  options: { etag: string | number; idempotencyKey?: string }
): Promise<ApiResult<Booking>> {
  return mutate<Booking>(
    `${BOOKINGS_PATH}/${encodeURIComponent(id)}/reschedule`,
    { method: "POST", json: input },
    { etag: options.etag, idempotencyKey: options.idempotencyKey }
  );
}

export interface CancelBookingInput {
  reason?: string;
}

/** 취소 요청 — 202 "canceling". 외부 취소 확인 전까지 완료로 보지 않는다 (§10.4). */
export function cancelBooking(
  id: string,
  input: CancelBookingInput,
  options: { etag: string | number; idempotencyKey?: string }
): Promise<ApiResult<Booking>> {
  return mutate<Booking>(
    `${BOOKINGS_PATH}/${encodeURIComponent(id)}/cancel`,
    { method: "POST", json: input },
    { etag: options.etag, idempotencyKey: options.idempotencyKey }
  );
}

export type BookingAttendanceOutcome = "attended" | "no_show";

export interface RecordBookingAttendanceInput {
  attendance_status: BookingAttendanceOutcome;
  outcome_note?: string;
}

/** Record a completed booking outcome; the API enforces host scope and end time. */
export function recordBookingAttendance(
  id: string,
  input: RecordBookingAttendanceInput,
  options: { etag: string | number; idempotencyKey?: string }
): Promise<ApiResult<Booking>> {
  return mutate<Booking>(
    `${BOOKINGS_PATH}/${encodeURIComponent(id)}/attendance`,
    { method: "POST", json: input },
    { etag: options.etag, idempotencyKey: options.idempotencyKey }
  );
}

// ── 보조 조회 (읽기 전용 — principals·calendar-connections) ──

export interface PrincipalRef {
  id: string;
  name?: string | null;
  display_name?: string | null;
  kind?: string;
  role?: string;
  status?: string;
  [key: string]: unknown;
}


export function principalDisplayName(principal: PrincipalRef | undefined): string | undefined {
  return principal?.name?.trim() || principal?.display_name?.trim() || undefined;
}

export const PRINCIPALS_PATH = "/api/v1/principals";

export interface CalendarConnectionRef {
  id: string;
  name?: string | null;
  host_principal_id?: string;
  provider?: string;
  provider_calendar_id?: string;
  status?: string;
  include_in_busy?: boolean;
  [key: string]: unknown;
}

export const CALENDAR_CONNECTIONS_PATH = "/api/v1/calendar-connections";
