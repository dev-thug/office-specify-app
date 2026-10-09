/**
 * 메시지 네임스페이스 전체 목록 — messages/<locale>/<namespace>.json 과 1:1 대응.
 *
 * 모든 네임스페이스를 여기서만 열거한다. 기능 작업자는 자기 영역 파일
 * (messages/<locale>/<area>.json)에 키만 추가하고 이 목록은 수정하지 않는다.
 */
export const NAMESPACES = [
  // 공통·셸
  "app",
  "nav",
  "common",
  "status",
  "auth",
  "screens",
  "public",
  "design",
  // 기능 영역
  "dashboard",
  "operations",
  "settings",
  "companies",
  "contacts",
  "inbox",
  "templates",
  "campaigns",
  "approvals",
  "meetings",
  "booking",
  "compliance",
  "agents",
  "imports",
  "research",
] as const;

export type Namespace = (typeof NAMESPACES)[number];
