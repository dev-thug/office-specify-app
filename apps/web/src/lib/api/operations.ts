/**
 * 운영 API (B.2 "예약·준수·운영" 표 + §16 + UI13).
 *
 * 부록 B에 명시된 것:
 * - `GET  /jobs/{id}`                    — 진행·에러·재시도 가능 여부
 * - `POST /jobs/{id}/retry-requests`     — 안전한 작업만; unknown은 대조 필요
 * - `GET  /audit-logs`                   — redacted 로그·cursor
 * - `GET  /dashboard/summary`            — dashboard.ts 참조
 * - `PUT  /operations/emergency-stop`    — enabled·이유·If-Match → 즉시 신규 제출 중지
 *
 * 부록 B에 없어 UI13 요구를 충족하기 위해 가정한 경로 (계약 확정 시 이 파일만 수정):
 * - `GET /jobs`                          — 작업 목록 (state/kind 필터, cursor)
 * - `GET /operations/emergency-stop`     — 현재 상태·버전 (If-Match 선행값 확보용)
 */
import { apiFetch, type ApiResult } from "./client";
import { mutate, type MutateOptions } from "./mutate";
import { useCursorList, type ListParams } from "./use-cursor-list";

// ── 작업 큐 (jobs DDL CHECK: queued/running/retry_wait/completed/dead/canceled/unknown) ──

export type JobState =
  | "queued"
  | "running"
  | "retry_wait"
  | "completed"
  | "dead"
  | "canceled"
  | "unknown";

export interface Job {
  id: string;
  kind: string;
  state: JobState | string;
  payload_version: number;
  attempt_count: number;
  last_error_code: string | null;
  available_at: string;
  lease_owner: string | null;
  lease_expires_at: string | null;
  requested_by: string | null;
  finished_at: string | null;
  created_at: string;
  /**
   * 서버가 내려주는 재시도 가능 여부 (GET /jobs/{id} 의 "재시도 가능 여부").
   * 목록 행에 없을 수 있다 — 없으면 재시도를 표시하지 않는다.
   */
  retryable?: boolean;
  [key: string]: unknown;
}

/** 관련 리소스 참조 (operations_get_job 의 "result refs"). */
export interface JobResultRef {
  type: string;
  id: string;
}

export interface JobDetail extends Job {
  retry_block_reason?: string | null;
  result_refs?: JobResultRef[];
  error_detail?: string | null;
  /** 외부 제출 경계가 있는 작업 — unknown이면 자동 재시도가 절대 금지다 (§16.4). */
  external_effect?: boolean;
}

export interface JobListFilters {
  state?: JobState | "";
  kind?: string;
}

/** `GET /api/v1/jobs` — 부록 B 미명시 가정 (상세 참조). */
export function useJobs(filters?: JobListFilters) {
  const params: ListParams = {
    state: filters?.state || undefined,
    kind: filters?.kind?.trim() || undefined,
  };
  return useCursorList<Job>({
    queryKey: ["operations", "jobs", params],
    path: "/api/v1/jobs",
    params,
  });
}

export function getJob(id: string): Promise<ApiResult<JobDetail>> {
  return apiFetch<JobDetail>(`/api/v1/jobs/${encodeURIComponent(id)}`);
}

export interface JobRetryRequestInput {
  reason?: string;
}

/**
 * 재시도 요청 — UI 측에서 unknown 상태에는 절대 이 경로를 호출하지 않는다
 * (B.2: "unknown은 대조 필요"). 호출 가능 상태 판단은 features/operations/job-rules.
 */
export function requestJobRetry(
  id: string,
  input: JobRetryRequestInput = {},
  options?: MutateOptions
) {
  return mutate(`/api/v1/jobs/${encodeURIComponent(id)}/retry-requests`, {
    method: "POST",
    json: input,
  }, options);
}

// ── 감사 로그 (audit_logs DDL — details는 허용 목록만 담은 redacted JSON) ──

export interface AuditLogEntry {
  id: string;
  actor_principal_id: string | null;
  requested_by: string | null;
  approved_by: string | null;
  job_id: string | null;
  action: string;
  target_type: string;
  target_id: string | null;
  request_id: string;
  trace_id: string | null;
  details: Record<string, unknown>;
  occurred_at: string;
  [key: string]: unknown;
}

export interface AuditLogFilters {
  action?: string;
  target_type?: string;
}

export function useAuditLogs(filters?: AuditLogFilters) {
  const params: ListParams = {
    action: filters?.action?.trim() || undefined,
    target_type: filters?.target_type?.trim() || undefined,
  };
  return useCursorList<AuditLogEntry>({
    queryKey: ["operations", "audit-logs", params],
    path: "/api/v1/audit-logs",
    params,
  });
}

// ── 긴급 중지 (workspaces.emergency_stop — PUT만 B.2 명시, GET은 가정) ──

export interface EmergencyStopState {
  enabled: boolean;
  version: number;
  reason: string | null;
  updated_at: string | null;
  updated_by: string | null;
  [key: string]: unknown;
}

/** `GET /api/v1/operations/emergency-stop` — 부록 B 미명시 가정 (If-Match 선행값). */
export function getEmergencyStop(): Promise<ApiResult<EmergencyStopState>> {
  return apiFetch<EmergencyStopState>("/api/v1/operations/emergency-stop");
}

export interface EmergencyStopInput {
  enabled: boolean;
  /** 감사 사유 — 활성화·해제 모두 필수로 받는다. */
  reason: string;
}

export function putEmergencyStop(
  input: EmergencyStopInput,
  options?: MutateOptions
) {
  return mutate<EmergencyStopState>(
    "/api/v1/operations/emergency-stop",
    { method: "PUT", json: input },
    options
  );
}
