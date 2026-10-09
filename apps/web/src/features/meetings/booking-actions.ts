/**
 * 예약 상태 → 허용 행동 가드 (§10.3–10.4, UI09 안전 규칙).
 *
 * - confirming / canceling: 진행 중 saga — 액션을 열지 않는다.
 * - unknown: 공급자 결과 불명 — 점유 유지·조사 대상이므로 변경 액션 없음.
 *   (안전 규칙: unknown은 "다시 시도/재발송"이 아니라 "조사"다.)
 * - failed / canceled / expired: 종결 — 변경 없음.
 * - held: 공급자 이벤트 생성 전 임시 점유 — 취소만 허용한다.
 * - confirmed: 일정 변경·취소; attendance is available only after the booked end time.
 *
 * 서버가 allowed_actions 힌트를 주면 그 안에서만 보인다 — 버튼 표시가 권한
 * 보장은 아니며 서버가 다시 검사한다 (§12.2, spec §4.10).
 */
import type { Booking } from "@/lib/api/meetings";

type ActionName = "reschedule" | "cancel" | "attendance";

function allows(b: Pick<Booking, "allowed_actions">, action: ActionName): boolean {
  // 힌트가 없으면 상태만으로 판단한다 — 있으면 교집합.
  return !b.allowed_actions || b.allowed_actions.includes(action);
}

export function bookingCanReschedule(
  b: Pick<Booking, "status" | "allowed_actions">
): boolean {
  return b.status === "confirmed" && allows(b, "reschedule");
}

export function bookingCanCancel(
  b: Pick<Booking, "status" | "allowed_actions">
): boolean {
  return (b.status === "confirmed" || b.status === "held") && allows(b, "cancel");
}

export function bookingCanRecordAttendance(
  b: Pick<Booking, "status" | "ends_at" | "allowed_actions">
): boolean {
  const ended = Number.isFinite(Date.parse(b.ends_at)) && Date.parse(b.ends_at) <= Date.now();
  return b.status === "confirmed" && ended && allows(b, "attendance");
}

/** 아직 진행 중이라 자동 새로고침·대기 안내가 필요한 상태. */
export function bookingInFlight(status: string): boolean {
  return status === "held" || status === "confirming" || status === "canceling";
}

/** 공급자 결과를 확인할 수 없어 운영 조사가 필요한 상태. */
export function bookingNeedsInvestigation(status: string): boolean {
  return status === "unknown";
}
