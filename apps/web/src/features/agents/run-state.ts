/**
 * agent_runs 상태 규칙 — DDL CHECK ('queued','running','awaiting_approval',
 * 'completed','failed','canceled') 기준. 컴포넌트·테스트가 공유한다.
 */
import type { AgentRunStatus } from "@/lib/api/agents";

/** 아직 진행 중인 상태 — 중단 요청·자동 새로고침 대상. */
export const ACTIVE_RUN_STATUSES: ReadonlySet<string> = new Set([
  "queued",
  "running",
  "awaiting_approval",
]);

export function isActiveRunStatus(status: AgentRunStatus | string): boolean {
  return ACTIVE_RUN_STATUSES.has(status);
}

export function isFinalRunStatus(status: AgentRunStatus | string): boolean {
  return status === "completed" || status === "failed" || status === "canceled";
}

/**
 * 중단 버튼 표시 조건 — 진행 중이고 아직 중단 요청이 없으며, 서버가
 * allowed_actions 를 보냈다면 "cancel" 을 포함해야 한다 (화면 힌트일 뿐
 * 서버 검사를 대신하지 않는다 — §12.2).
 */
export function canCancelRun(run: {
  status: string;
  cancel_requested_at?: string | null;
  allowed_actions?: string[];
}): boolean {
  if (!isActiveRunStatus(run.status)) return false;
  if (run.cancel_requested_at) return false;
  if (run.allowed_actions && !run.allowed_actions.includes("cancel")) return false;
  return true;
}
