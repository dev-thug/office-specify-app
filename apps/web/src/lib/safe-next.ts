/**
 * 로그인 후 복귀 경로 검증 — open redirect 방지 (C1).
 * 허용: 같은 출처의 /app/** 상대 경로만. 그 외는 /app/dashboard.
 */
export const DEFAULT_AFTER_LOGIN = "/app/dashboard";

export function safeNextPath(next: string | null | undefined): string {
  // /app 또는 /app/… 만 허용 — "/application" 같은 접두 우회 차단
  if (
    !next ||
    !(next === "/app" || next.startsWith("/app/")) ||
    next.startsWith("//")
  ) {
    return DEFAULT_AFTER_LOGIN;
  }
  try {
    // javascript:, 외부 호스트, 프로토콜 상대 등 모두 차단
    // (브라우저 기준은 location.origin — 여기서는 고정 origin으로 동등 보장)
    const url = new URL(next, "https://placeholder.invalid");
    if (url.origin !== "https://placeholder.invalid") {
      return DEFAULT_AFTER_LOGIN;
    }
    // 브라우저 탐색 전에 디코드+정규화를 반복해 "/app/../x", "%2e%2e" 등의
    // 경로 탈출을 차단한다 — 최종 pathname 이 /app 아래여야 한다.
    let pathname = url.pathname;
    for (let i = 0; i < 3; i++) {
      let decoded: string;
      try {
        decoded = decodeURIComponent(pathname);
      } catch {
        decoded = pathname;
      }
      if (decoded === pathname) break;
      pathname = new URL(decoded, "https://placeholder.invalid").pathname;
    }
    if (pathname === "/app" || pathname.startsWith("/app/")) {
      return next;
    }
  } catch {
    // URL 파싱 불가 → 허용하지 않는다
  }
  return DEFAULT_AFTER_LOGIN;
}
