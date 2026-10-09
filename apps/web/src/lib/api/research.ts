/**
 * research 영역 API — lead_proposals·research_evidence.
 * B.2 3564–3568행, DDL(lead_proposals 1401행, research_evidence 1421행) 기준 손 타이핑.
 *
 * 엔드포인트:
 * - GET  /api/v1/lead-proposals               (pending·중복 후보 목록)
 * - POST /api/v1/lead-proposals/{id}/decisions (accept/reject·병합 선택 + If-Match)
 * - GET  /api/v1/research-evidence            (목록 — B.2 미명시, 회사별 조회 가정)
 * - POST /api/v1/research-evidence/{id}/reviews (verified/rejected·이유)
 *
 * 안전 규칙 (§13.4): 채택된 후보의 이메일도 communication_basis=pending 으로
 * 시작한다 — UI는 발송 가능으로 표시하지 않는다.
 */
import { apiFetch, type ApiResult } from "./client";
import { mutate } from "./mutate";
import { useCursorList, type ListParams } from "./use-cursor-list";

// ── DTO ────────────────────────────────────────────────────

/** research_evidence.review_status CHECK. */
export type EvidenceReviewStatus = "pending" | "verified" | "rejected";
/** research_evidence.evidence_type CHECK — 사실·추론 구분 (§13.4). */
export type EvidenceType = "observed" | "inferred";
/** lead_proposals.status CHECK. */
export type LeadProposalStatus =
  | "pending"
  | "accepted"
  | "rejected"
  | "needs_review";

export interface ResearchEvidence {
  id: string;
  company_id: string | null;
  contact_id: string | null;
  lead_proposal_id: string | null;
  field_path: string;
  source_url: string;
  observed_at: string;
  excerpt: string | null;
  /** JSONB — 문자열이 대부분이지만 임의 값을 허용한다. */
  value: unknown;
  evidence_type: EvidenceType;
  /** 모델 확신도 — 검증 완료 표시를 대신하지 않는다 (§13.4). */
  confidence: number | null;
  review_status: EvidenceReviewStatus;
  created_by: string;
  reviewed_by: string | null;
  created_at: string;
}

export interface LeadProposal {
  id: string;
  agent_run_id: string | null;
  proposed_by: string;
  external_key: string | null;
  company_name: string;
  domain: string | null;
  proposed_data: Record<string, unknown>;
  status: LeadProposalStatus;
  accepted_company_id: string | null;
  reviewed_by: string | null;
  reviewed_at: string | null;
  version: number;
  created_at: string;
  /** B.3.12: 목록 응답에 증빙·기존 회사 중복 후보 ID 를 동봉한다고 가정. */
  evidence?: ResearchEvidence[];
  duplicate_candidates?: string[];
  allowed_actions?: string[];
}

export type LeadDecision = "accept" | "reject";

export interface DecideLeadProposalInput {
  decision: LeadDecision;
  /** accept 시 기존 기업에 병합할 대상 — duplicate_candidates 중 하나 (병합 선택). */
  merge_into_company_id?: string;
  reason?: string;
}

export type EvidenceReviewVerdict = "verified" | "rejected";

export interface ReviewEvidenceInput {
  review_status: EvidenceReviewVerdict;
  reason?: string;
}

// ── 경로 ────────────────────────────────────────────────────

export const LEAD_PROPOSALS_PATH = "/api/v1/lead-proposals";
export const RESEARCH_EVIDENCE_PATH = "/api/v1/research-evidence";

export function leadProposalDecisionsPath(id: string): string {
  return `${LEAD_PROPOSALS_PATH}/${encodeURIComponent(id)}/decisions`;
}

export function researchEvidenceReviewsPath(id: string): string {
  return `${RESEARCH_EVIDENCE_PATH}/${encodeURIComponent(id)}/reviews`;
}

// ── 쿼리 훅·뮤테이션 ─────────────────────────────────────────

/** 후보 목록 — status는 서버 필터, agent-run scope는 아직 응답 페이지 안에서 필터한다. */
export function useLeadProposals(params?: {
  status?: LeadProposalStatus | "all";
  agentRunId?: string;
}) {
  return useCursorList<LeadProposal>({
    queryKey: [
      "lead-proposals",
      params?.status ?? "all",
      params?.agentRunId ?? null,
    ],
    path: LEAD_PROPOSALS_PATH,
    params: {
      status:
        params?.status && params.status !== "all" ? params.status : undefined,
    } satisfies ListParams,
  });
}

/**
 * 인간 채택·거절 — lead_proposals.version 을 If-Match 로 보낸다 (B.2: If-Match 명시).
 * 성공 시 accepted 면 crm activity projector 가 lead.accepted 를 소비한다.
 */
export function decideLeadProposal(
  id: string,
  input: DecideLeadProposalInput,
  options: { etag: number | string; idempotencyKey?: string }
): Promise<ApiResult<LeadProposal>> {
  return mutate<LeadProposal>(
    leadProposalDecisionsPath(id),
    { method: "POST", json: input },
    { etag: options.etag, idempotencyKey: options.idempotencyKey }
  );
}

/** 회사 범위 증빙 목록 — 읽기 경로는 B.2에 없어 ?company_id= 조회를 가정 (report 참조). */
export function useCompanyResearchEvidence(params: {
  companyId: string;
  reviewStatus?: EvidenceReviewStatus | "all";
  enabled?: boolean;
}) {
  return useCursorList<ResearchEvidence>({
    queryKey: [
      "research-evidence",
      params.companyId,
      params.reviewStatus ?? "all",
    ],
    path: RESEARCH_EVIDENCE_PATH,
    params: {
      company_id: params.companyId,
      review_status:
        params.reviewStatus && params.reviewStatus !== "all"
          ? params.reviewStatus
          : undefined,
    } satisfies ListParams,
    enabled: params.enabled,
  });
}

export function getResearchEvidence(id: string): Promise<ApiResult<ResearchEvidence>> {
  return apiFetch<ResearchEvidence>(
    `${RESEARCH_EVIDENCE_PATH}/${encodeURIComponent(id)}`
  );
}

/** 인간 증빙 검토 — research_evidence 에 version 이 없어 If-Match 없이 보낸다. */
export function reviewResearchEvidence(
  id: string,
  input: ReviewEvidenceInput,
  options?: { idempotencyKey?: string }
): Promise<ApiResult<ResearchEvidence>> {
  return mutate<ResearchEvidence>(
    researchEvidenceReviewsPath(id),
    { method: "POST", json: input },
    { idempotencyKey: options?.idempotencyKey }
  );
}
