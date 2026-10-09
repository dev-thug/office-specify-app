/**
 * 대시보드 요약 API (B.2: `GET /dashboard/summary` — "KPI 정의·cohort·freshness 포함").
 *
 * `crates/contracts/src/operations.rs` wire DTO를 그대로 타입화한다. KPI 값은
 * summary object를 UI 카드에 맞춰 별도 어댑터로 바꾸되 서버의 분모·기간·freshness를 보존한다.
 */
import { apiFetch, type ApiResult } from "./client";

/** RFC 3339 관측 구간. */
export interface DashboardWindow {
  from: string;
  to: string;
}

/** §2.4 지표 단위 — 비율은 0..1, 시간은 초. */
export type DashboardKpiUnit = "count" | "ratio" | "seconds";

export interface DashboardKpi {
  /** 지표 식별자 — 화면은 messages.dashboard.kpi.<key> 로 라벨을 해석한다. */
  key: string;
  /** 서버 제공 라벨(선택) — 메시지 키가 없을 때 폴백. */
  label?: string;
  unit: DashboardKpiUnit | string;
  /** null → 관측 불가. 화면은 반드시 N/A로 표시한다 (추정·0 대체 금지). */
  value: number | null;
  numerator?: number | null;
  /** 분모 — UI01이 카드에 명시를 요구한다. */
  denominator?: number | null;
  /** Median KPI 표본 수 — denominator와 구분해 표시한다. */
  sample_size?: number | null;
  window?: DashboardWindow;
  refreshed_at?: string | null;
  [key: string]: unknown;
}

export interface DashboardRatioKpi {
  numerator: number;
  denominator: number;
  value: number | null;
}

export interface DashboardMedianKpi {
  sample_size: number;
  median_seconds: number | null;
}

export interface DashboardKpis {
  eligible_contacted_companies: number;
  reply_company_ratio: DashboardRatioKpi;
  meeting_conversion: DashboardRatioKpi;
  first_reply: DashboardMedianKpi;
  agent_proposal_adoption: DashboardRatioKpi;
}

export interface DashboardOperational {
  unknown_outbound_messages: number;
  unknown_jobs: number;
  dead_jobs: number;
  outbox_backlog: number;
  reauth_required_mailboxes: number;
  emergency_stop: boolean;
  send_workers_locked: boolean;
}

/** "오늘 처리할 일" 큐 — key 는 이동 대상 영역 식별자다. */
export interface DashboardQueue {
  key: string;
  count: number;
  oldest_at?: string | null;
  [key: string]: unknown;
}

export interface DashboardSummary {
  period: DashboardWindow;
  generated_at: string;
  scope: DashboardScope;
  kpis: DashboardKpis;
  operational: DashboardOperational;
  queues: DashboardQueue[];
  [key: string]: unknown;
}

/** `GET /dashboard/summary` accepts team scope or caller-owned mine scope. */
export type DashboardScope = "mine" | "team";

export function getDashboardSummary(params?: {
  scope?: DashboardScope;
}): Promise<ApiResult<DashboardSummary>> {
  const qs = params?.scope
    ? `?scope=${encodeURIComponent(params.scope)}`
    : "";
  return apiFetch<DashboardSummary>(`/api/v1/dashboard/summary${qs}`);
}
