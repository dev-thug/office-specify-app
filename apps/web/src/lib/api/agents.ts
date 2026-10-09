/**
 * agents 영역 API — agent_runs·agent_actions·agent principal.
 * contracts/openapi 생성 전이라 소스 부록 B(B.2 3632–3634행)와 DDL(agent_runs
 * 1383행, agent_actions 1991행)의 필드명을 그대로 손으로 타이핑한다.
 *
 * 엔드포인트:
 * - GET/POST /api/v1/agent-runs          (목록 / 목적·budget·대상 범위 → 201/202)
 * - GET      /api/v1/agent-runs/{id}     (action·진행·오류·승인 포함 상세)
 * - POST     /api/v1/agent-runs/{id}/cancel (새 도구 실행 중단)
 * - GET      /api/v1/principals          (kind=agent 인 주체 목록 — 클라이언트 필터)
 */
import { apiFetch, type ApiResult } from "./client";
import { mutate } from "./mutate";
import { useCursorList, type ListParams } from "./use-cursor-list";

// ── DTO (DDL CHECK 집합 그대로) ──────────────────────────────

/** agent_runs.status CHECK — DDL. */
export type AgentRunStatus =
  | "queued"
  | "running"
  | "awaiting_approval"
  | "completed"
  | "failed"
  | "canceled";

/**
 * agent_runs.budget/usage JSONB — §13.5 상한 종류(도구 호출 수·후보 수·외부 검색
 * 요청 수·입출력 토큰·비용 예산)를 알려진 키로 타입화하고 나머지는 통과시킨다.
 * 비용은 공급자 과금 자료가 없으면 추정치다 (실측 actual 과 추정 estimated 구분).
 */
export interface AgentRunBudget {
  max_tool_calls?: number;
  max_proposals?: number;
  max_external_searches?: number;
  max_input_tokens?: number;
  max_output_tokens?: number;
  max_cost_usd?: number;
  [key: string]: unknown;
}

export interface AgentRunUsage {
  tool_calls?: number;
  proposals_submitted?: number;
  external_searches?: number;
  input_tokens?: number;
  output_tokens?: number;
  estimated_cost_usd?: number;
  actual_cost_usd?: number;
  [key: string]: unknown;
}

export interface AgentRun {
  id: string;
  principal_id: string;
  requested_by: string;
  source_credential_id: string | null;
  objective: string;
  status: AgentRunStatus;
  budget: AgentRunBudget;
  usage: AgentRunUsage;
  started_at: string | null;
  finished_at: string | null;
  cancel_requested_at: string | null;
  created_at: string;
  /** 화면 힌트 — 서버 재검사를 대신하지 않는다 (§12.2). */
  allowed_actions?: string[];
}

/** agent_actions — 민감 원문은 저장하지 않고 인자는 마스킹본+해시만 온다 (§13.5). */
export interface AgentAction {
  id: string;
  agent_run_id: string;
  tool_name: string;
  /** 응답은 SHA-256 hex 64자 (B.1.1 Hash). */
  arguments_hash: string;
  redacted_arguments: Record<string, unknown>;
  result_code: string | null;
  result_ref: string | null;
  approval_request_id: string | null;
  has_external_effect: boolean;
  trace_id: string | null;
  started_at: string;
  finished_at: string | null;
  created_at: string;
}

/** GET /agent-runs/{id} 의 data — actions 는 상세에만 동봉된다고 가정 (B.2 비명시). */
export interface AgentRunDetail extends AgentRun {
  actions?: AgentAction[];
}

/** run 요청 입력 — 목적·budget·대상 범위 (B.2). */
export interface CreateAgentRunInput {
  agent_principal_id: string;
  objective: string;
  scope?: {
    industry?: string;
    country_code?: string;
    [key: string]: unknown;
  };
  budget?: AgentRunBudget;
}

/** principals 목록 행 — kind=agent 인 행만 에이전트 목록으로 사용한다. */
export interface AgentPrincipal {
  id: string;
  kind: string;
  display_name: string;
  status: string;
  role?: string;
  scopes?: string[];
  [key: string]: unknown;
}

// ── 경로 ────────────────────────────────────────────────────

export const AGENT_RUNS_PATH = "/api/v1/agent-runs";
export const PRINCIPALS_PATH = "/api/v1/principals";

export function agentRunPath(id: string): string {
  return `${AGENT_RUNS_PATH}/${encodeURIComponent(id)}`;
}

export function agentRunCancelPath(id: string): string {
  return `${agentRunPath(id)}/cancel`;
}

// ── 쿼리 훅·뮤테이션 ─────────────────────────────────────────

/** 실행 목록 — status 필터는 서버 allowlist 에 맡기고 그대로 전달한다. */
export function useAgentRuns(params?: { status?: AgentRunStatus | "all" }) {
  return useCursorList<AgentRun>({
    queryKey: ["agent-runs", params?.status ?? "all"],
    path: AGENT_RUNS_PATH,
    params: {
      status:
        params?.status && params.status !== "all" ? params.status : undefined,
    } satisfies ListParams,
  });
}

export function getAgentRun(id: string): Promise<ApiResult<AgentRunDetail>> {
  return apiFetch<AgentRunDetail>(agentRunPath(id));
}

/** run 요청 — 비동기 생성이므로 Idempotency-Key 필수 (B.1.3). */
export function createAgentRun(
  input: CreateAgentRunInput,
  options?: { idempotencyKey?: string }
): Promise<ApiResult<AgentRun>> {
  return mutate<AgentRun>(AGENT_RUNS_PATH, {
    method: "POST",
    json: input,
  }, { idempotencyKey: options?.idempotencyKey });
}

/**
 * 실행 중단 — 새 도구 실행을 막는다. 이미 제출된 외부 효과는 되돌리지 않으며
 * 서버가 별도로 표시한다 (B.2 3634행). agent_runs 에는 version 이 없어
 * If-Match 는 보내지 않는다.
 */
export function cancelAgentRun(
  id: string,
  input: { reason?: string },
  options?: { idempotencyKey?: string }
): Promise<ApiResult<AgentRun>> {
  return mutate<AgentRun>(agentRunCancelPath(id), {
    method: "POST",
    json: input,
  }, { idempotencyKey: options?.idempotencyKey });
}

/** 에이전트 principal 목록 — kind=agent 만 남긴다 (필터 파라미터는 미명시라 클라에서 적용). */
export function useAgentPrincipals() {
  return useCursorList<AgentPrincipal>({
    queryKey: ["principals", "agent"],
    path: PRINCIPALS_PATH,
    params: { kind: "agent" },
  });
}
