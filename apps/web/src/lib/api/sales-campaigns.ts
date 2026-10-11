import { apiFetch, type ApiResult } from "./client";
import { mutate } from "./mutate";
import type { MutateOptions } from "./mutate";
import type { MessageDraftResponse } from "./sending";

export const SALES_CAMPAIGNS_PATH = "/api/v1/sales-campaigns";
export const SALES_CAMPAIGN_SENDER_PATH = `${SALES_CAMPAIGNS_PATH}/sender`;

export interface SalesCampaignSenderSummary {
  smtp_profile_id: string;
  profile_name: string;
  mail_account_id: string;
  mailbox_name: string;
  delivery_channel_id: string;
  sender_identity_id: string;
  from_address: string;
  from_name: string;
  reply_to: string | null;
  status: string;
  ready: boolean;
}

export interface SalesCampaignSenderResponse {
  sender: SalesCampaignSenderSummary | null;
}

export interface SalesCampaignSummary {
  id: string;
  name: string;
  objective: string | null;
  mail_account_id: string;
  delivery_channel_id: string;
  sender_identity_id: string;
  smtp_profile_id: string;
  meeting_type_id: string | null;
  from_address: string;
  from_name: string;
  reply_to: string | null;
  sender_status: string;
  version: number;
  created_at: string;
  updated_at: string;
}

export interface SalesCampaignDraftSummary {
  draft_id: string;
  subject: string;
  status: string;
  version: number;
  outbound_message_id: string | null;
}

export interface SalesCampaignOutboundSummary {
  outbound_message_id: string;
  status: string;
  payload_hash: string;
  approval_status: string | null;
  approval_expires_at: string | null;
  approved_at: string | null;
  provider_accepted: boolean;
  accepted_at: string | null;
  failure_code: string | null;
  open_count: number;
  click_count: number;
  known_reply_count: number;
  last_reply_at: string | null;
  not_before?: string | null;
  batch_at?: string | null;
}

export interface SalesCampaignTarget {
  id: string;
  company_id: string | null;
  contact_id: string | null;
  email_address_id: string | null;
  company_name: string | null;
  contact_name: string | null;
  email_address: string | null;
  stage: string;
  workflow_stage: string;
  version: number;
  current_draft: SalesCampaignDraftSummary | null;
  outbound: SalesCampaignOutboundSummary | null;
  created_at: string;
  updated_at: string;
}

export interface SalesCampaignResponse {
  campaign: SalesCampaignSummary;
  targets: SalesCampaignTarget[];
}

export interface CreateSalesCampaignInput {
  name: string;
  objective?: string | null;
  meeting_type_id?: string | null;
}

export interface AddSalesCampaignTargetInput {
  company_id: string;
  contact_id: string;
  email_address_id: string;
}

export type SalesCampaignWorkflowStage = "research" | "draft" | "review";

export interface UpdateSalesCampaignTargetInput {
  stage: SalesCampaignWorkflowStage;
  expected_target_version: number;
}

export interface CreateSalesCampaignTargetDraftInput {
  subject: string;
  body_text: string;
  body_html?: string | null;
  template_version_id?: string | null;
  variables?: Record<string, unknown> | null;
  explicit_follow_up?: boolean;
}

export interface QueueSalesCampaignTargetInput {
  outbound_message_id: string;
  expected_payload_hash: string;
  expected_target_version: number;
}

export interface QueueSalesCampaignTargetResponse {
  submission: {
    outbound_message_id: string;
    status: string;
    job_id: string;
    provider_accepted: boolean;
  };
  batch_at: string;
}

type Idempotent = Pick<MutateOptions, "idempotencyKey">;

function targetPath(campaignId: string, targetId: string) {
  return `${SALES_CAMPAIGNS_PATH}/${encodeURIComponent(campaignId)}/targets/${encodeURIComponent(targetId)}`;
}

export function getSalesCampaignSender(): Promise<ApiResult<SalesCampaignSenderResponse>> {
  return apiFetch<SalesCampaignSenderResponse>(SALES_CAMPAIGN_SENDER_PATH);
}

export function setDefaultSalesCampaignSender(
  smtpProfileId: string,
  options?: Idempotent
): Promise<ApiResult<SalesCampaignSenderResponse>> {
  return mutate<SalesCampaignSenderResponse>(
    SALES_CAMPAIGN_SENDER_PATH,
    { method: "PUT", json: { smtp_profile_id: smtpProfileId } },
    options
  );
}

export function listSalesCampaigns(params: {
  limit?: number;
  cursor?: string;
} = {}): Promise<ApiResult<SalesCampaignSummary[]>> {
  const query = new URLSearchParams();
  query.set("limit", String(params.limit ?? 50));
  if (params.cursor) query.set("cursor", params.cursor);
  return apiFetch<SalesCampaignSummary[]>(`${SALES_CAMPAIGNS_PATH}?${query}`);
}

export function getSalesCampaign(id: string): Promise<ApiResult<SalesCampaignResponse>> {
  return apiFetch<SalesCampaignResponse>(
    `${SALES_CAMPAIGNS_PATH}/${encodeURIComponent(id)}`
  );
}

/** `POST /sales-campaigns` 는 카드 목록 없이 캠페인 요약(SalesCampaignSummary)만 돌려준다. */
export function createSalesCampaign(
  input: CreateSalesCampaignInput,
  options?: Idempotent
): Promise<ApiResult<SalesCampaignSummary>> {
  return mutate<SalesCampaignSummary>(
    SALES_CAMPAIGNS_PATH,
    { method: "POST", json: input },
    options
  );
}

export function addSalesCampaignTarget(
  campaignId: string,
  input: AddSalesCampaignTargetInput,
  options?: Idempotent
): Promise<ApiResult<SalesCampaignTarget>> {
  return mutate<SalesCampaignTarget>(
    `${SALES_CAMPAIGNS_PATH}/${encodeURIComponent(campaignId)}/targets`,
    { method: "POST", json: input },
    options
  );
}

export function updateSalesCampaignTarget(
  campaignId: string,
  targetId: string,
  input: UpdateSalesCampaignTargetInput,
  options?: Idempotent
): Promise<ApiResult<SalesCampaignTarget>> {
  return mutate<SalesCampaignTarget>(
    targetPath(campaignId, targetId),
    { method: "PATCH", json: input },
    options
  );
}

export function createSalesCampaignTargetDraft(
  campaignId: string,
  targetId: string,
  input: CreateSalesCampaignTargetDraftInput,
  options?: Idempotent
): Promise<ApiResult<MessageDraftResponse>> {
  return mutate<MessageDraftResponse>(
    `${targetPath(campaignId, targetId)}/drafts`,
    { method: "POST", json: input },
    options
  );
}

export function queueSalesCampaignTarget(
  campaignId: string,
  targetId: string,
  input: QueueSalesCampaignTargetInput,
  options?: Idempotent
): Promise<ApiResult<QueueSalesCampaignTargetResponse>> {
  return mutate<QueueSalesCampaignTargetResponse>(
    `${targetPath(campaignId, targetId)}/queue`,
    { method: "POST", json: input },
    options
  );
}
