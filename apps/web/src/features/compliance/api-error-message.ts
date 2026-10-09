import { ApiError } from "@/lib/api/problem";

/** messages 키를 string으로 받는 번역 함수 (ko/en parity는 messages.test.ts가 검사). */
export type Translate = (key: string) => string;

/**
 * 뮤테이션 오류 → compliance 네임스페이스의 지역화 문구.
 * 안정 코드(B.5 + spec §4.3 확장)만 매핑하고, 나머지는 서버 detail/title을
 * 그대로 보여준다 — 그마저 없으면 generic.
 */
export function apiErrorMessage(t: Translate, error: unknown): string {
  if (error instanceof ApiError) {
    const { problem } = error;
    if (problem.status === 403 || problem.code === "SCOPE_REQUIRED")
      return t("errors.forbidden");
    if (
      problem.status === 412 ||
      problem.status === 428 ||
      problem.code === "VERSION_MISMATCH"
    )
      return t("errors.conflict");
    if (problem.code === "IDEMPOTENCY_KEY_REUSED")
      return t("errors.idempotencyReused");
    if (problem.code === "REQUEST_IN_PROGRESS")
      return t("errors.inProgress");
    if (problem.detail) return problem.detail;
    if (problem.title) return problem.title;
  }
  return t("errors.generic");
}
