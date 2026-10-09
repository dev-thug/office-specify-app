/**
 * ApiError → 로컬라이즈된 안내 문구. B.5 코드를 영역 메시지 키로 매핑한다.
 * t 는 호출자가 넘기는 번역 함수 (meetings/errors 또는 booking 네임스페이스).
 */
import { ApiError } from "@/lib/api/problem";

type Translate = (key: string) => string;

/** 내부 meetings 영역 오류 매핑 (meetings.errors.*). */
export function meetingsErrorMessage(err: unknown, t: Translate): string {
  if (err instanceof ApiError) {
    switch (err.code) {
      case "VERSION_MISMATCH":
        return t("conflict");
      case "VERSION_PRECONDITION_REQUIRED":
        return t("precondition");
      case "FORBIDDEN":
      case "SCOPE_REQUIRED":
      case "MAILBOX_ACCESS_DENIED":
        return t("forbidden");
      case "SLOT_UNAVAILABLE":
        return t("slotUnavailable");
      case "CALENDAR_AVAILABILITY_UNAVAILABLE":
      case "INTEGRATION_REAUTH_REQUIRED":
        return t("calendarUnavailable");
      case "RATE_LIMITED":
      case "ACCOUNT_LOCKED":
        return t("rateLimited");
      default:
        break;
    }
  }
  return t("generic");
}
