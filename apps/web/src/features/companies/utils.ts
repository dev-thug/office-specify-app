/**
 * CRM 화면 공용 유틸 — 회사/담당자 양쪽 기능이 함께 쓴다.
 */
import type { CompanyActivity } from "@/lib/api/companies";

/** RFC 3339 → 로케일 표시. 파싱 불가 값은 원문을 돌려준다. */
export function formatDateTime(
  iso: string | null | undefined,
  locale: string
): string {
  if (!iso) return "";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return new Intl.DateTimeFormat(locale, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(d);
}

export function formatDate(
  iso: string | null | undefined,
  locale: string
): string {
  if (!iso) return "";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return new Intl.DateTimeFormat(locale, { dateStyle: "medium" }).format(d);
}

/** 기한 지남 여부 — open 상태이고 due_at이 과거면 true. */
export function isOverdue(task: {
  status: string;
  due_at: string | null;
}): boolean {
  if (task.status !== "open" || !task.due_at) return false;
  const d = new Date(task.due_at);
  return !Number.isNaN(d.getTime()) && d.getTime() < Date.now();
}

/**
 * 활동 행을 표시용 라벨·분류로 변환한다.
 * summary 는 비민감 라벨만 담는다(메일 제목 등은 grant 통과 후 서버가 실어보낸다).
 */
export type ActivityGroup =
  | "mail"
  | "booking"
  | "note"
  | "task"
  | "research"
  | "suppression"
  | "approval"
  | "campaign"
  | "other";

export function activityGroup(a: CompanyActivity): ActivityGroup {
  if (a.thread_id != null || a.mail_account_id != null) return "mail";
  const k = a.kind ?? "";
  if (k.startsWith("booking") || a.booking_id != null) return "booking";
  if (k.startsWith("note")) return "note";
  if (k.startsWith("task")) return "task";
  if (k.startsWith("research") || k.startsWith("lead")) return "research";
  if (k.startsWith("suppression")) return "suppression";
  if (k.startsWith("approval")) return "approval";
  if (k.startsWith("campaign") || k.startsWith("outbound")) return "campaign";
  return "other";
}

/** summary 에서 화면에 보일 짧은 라벨을 고른다 — 민감 키(body 등)는 절대 렌더하지 않는다. */
export function activityLabel(a: CompanyActivity): string {
  const s = a.summary ?? {};
  for (const key of ["label", "title", "subject", "text", "reason"]) {
    const v = s[key];
    if (typeof v === "string" && v.trim().length > 0) return v;
  }
  return "";
}

/** summary 에 실려 오는 예약 상태(있다면 StatusBadge 표시용). */
export function activityStatus(a: CompanyActivity): string | null {
  const v = a.summary?.["status"];
  return typeof v === "string" && v ? v : null;
}
