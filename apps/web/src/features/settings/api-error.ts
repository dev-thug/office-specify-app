/**
 * ApiError.code(B.5) → 영역 메시지 매핑.
 * 알 수 없는 코드는 서버가 보낸 detail/title → 영역 generic 순으로 폴백한다.
 * 매핑은 표시용이며 request_id는 ErrorState/토스트에서 항상 함께 남긴다.
 */
import { ApiError } from "@/lib/api/problem";

export interface AreaTranslator {
  (key: string): string;
  has(key: string): boolean;
}

const CODE_TO_KEY: Record<string, string> = {
  VERSION_MISMATCH: "errors.conflict",
  VERSION_PRECONDITION_REQUIRED: "errors.precondition",
  REQUEST_IN_PROGRESS: "errors.inProgress",
  IDEMPOTENCY_KEY_REUSED: "errors.inProgress",
  WORKSPACE_EMERGENCY_STOP: "errors.emergencyStop",
  FORBIDDEN: "errors.forbidden",
  SCOPE_REQUIRED: "errors.forbidden",
  PRINCIPAL_DISABLED: "errors.forbidden",
  MAILBOX_ACCESS_DENIED: "errors.forbidden",
  AGENT_DIRECT_SEND_FORBIDDEN: "errors.agentGrantForbidden",
  SELF_APPROVAL_FORBIDDEN: "errors.forbidden",
  NOT_FOUND: "errors.notFound",
  PROVIDER_POLICY_BLOCKED: "errors.providerBlocked",
  PROVIDER_POLICY_UNVERIFIED: "errors.providerBlocked",
  INVALID_CREDENTIALS: "errors.invalidCredentials",
};

export function apiErrorText(t: AreaTranslator, error: unknown): string {
  if (error instanceof ApiError) {
    const key = CODE_TO_KEY[error.code];
    if (key && t.has(key)) return t(key);
    return error.problem.detail ?? error.problem.title;
  }
  if (error instanceof Error) return error.message;
  return t("errors.generic");
}
