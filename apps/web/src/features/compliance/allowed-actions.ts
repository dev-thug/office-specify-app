import type { CommunicationBasis, Suppression } from "@/lib/api/compliance";

/**
 * B.1.1 AllowedActions — 서버가 내려준 action 문자열 목록은 "화면 힌트"다.
 * 목록이 없으면 버튼을 보이고 서버 검사(403)에 맡긴다. 있으면 후보 중
 * 하나라도 있어야 한다 — action 문자열은 계약 미확정이라 동의어를 함께 본다.
 */
export function hasAction(
  row: Pick<Suppression | CommunicationBasis, "allowed_actions">,
  ...candidates: string[]
): boolean {
  if (row.allowed_actions === undefined) return true;
  return candidates.some((c) => row.allowed_actions!.includes(c));
}

/** 차단 해제는 별도 승인 요청 — 활성(미해제) 행에만 의미가 있다. */
export function canRequestRelease(row: Suppression): boolean {
  return (
    row.released_at === null &&
    hasAction(row, "release_request", "request_release", "release")
  );
}

/** 근거 검토는 pending 상태에만. */
export function canReviewBasis(row: CommunicationBasis): boolean {
  return row.status === "pending" && hasAction(row, "review");
}

/** 근거 철회는 valid 상태에만(만료·거절·이미 철회된 건은 철회 대상이 아니다). */
export function canRevokeBasis(row: CommunicationBasis): boolean {
  return row.status === "valid" && hasAction(row, "revoke");
}
