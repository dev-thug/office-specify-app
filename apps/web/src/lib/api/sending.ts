/**
 * Single-message sending API — source §8.3–8.5, §9.3 and B.3.10/B.3.11.
 * DTO field names follow the wire contract. This module deliberately keeps
 * freeze, approval, preflight and submit separate so callers can bind every
 * irreversible action to the exact frozen payload.
 */
import { apiFetch, type ApiResult } from "./client";
import { mutate, type MutateOptions } from "./mutate";

export const MESSAGE_DRAFTS_PATH = "/api/v1/message-drafts";
export const OUTBOUND_MESSAGES_PATH = "/api/v1/outbound-messages";

export interface CreateMessageDraftInput {
  mail_account_id: string;
  to_email_address_id: string;
  purpose: "marketing" | "correspondence" | "transactional" | string;
  subject: string;
  body_text: string;
  body_html?: string | null;
  template_version_id?: string | null;
  variables?: Record<string, unknown>;
  thread_id?: string | null;
  reply_to_message_id?: string | null;
  expected_last_inbound_id?: string | null;
  visibility?: "private" | "mailbox";
}

export interface MessageDraftListItem {
  draft_id: string;
  mail_account_id: string;
  owner_principal_id: string;
  visibility: string;
  thread_id?: string | null;
  outbound_message_id?: string | null;
  purpose: string;
  subject: string;
  status: string;
  version: number;
  created_at: string;
  updated_at: string;
  [key: string]: unknown;
}

export interface MessageDraftResponse extends MessageDraftListItem {
  reply_to_message_id?: string | null;
  expected_last_inbound_id?: string | null;
  to_email_address_id: string;
  template_version_id?: string | null;
  body_text: string;
  body_html?: string | null;
  variables?: Record<string, unknown>;
}

export interface FreezeMessageDraftInput {
  delivery_channel_id: string;
  sender_identity_id?: string;
  meeting_type_id?: string;
  tracking_requested?: boolean;
}

export interface FreezeMessageDraftResult {
  outbound_message_id: string;
  status: string;
  payload_hash: string;
  version: number;
}

export interface OutboundPreflightCheck {
  name: string;
  passed: boolean;
  code?: string | null;
}

export interface OutboundPreflightResponse {
  checked_at: string;
  is_execution_authorization: false;
  global_blocking_reasons: string[];
  checks: OutboundPreflightCheck[];
}

export interface CreateOutboundApprovalInput {
  reason: string;
  expected_payload_hash: string;
  expires_at?: string;
}

export interface CreateOutboundApprovalResult {
  approval_request_id: string;
  status: string;
  expires_at: string;
  version: number;
}

export interface SubmitOutboundInput {
  expected_payload_hash: string;
  not_before?: string;
}

export interface SubmitOutboundResult {
  outbound_message_id: string;
  status: string;
  job_id: string;
  provider_accepted: false;
}

export interface CancelOutboundInput {
  reason?: string;
}

export interface CancelOutboundResult {
  outbound_message_id: string;
  status: string;
  version: number;
}

export interface OutboundEngagementCounts {
  tracking_enabled: boolean;
  open_request_count: number;
  click_request_count: number;
}

export interface OutboundKnownReply {
  inbound_message_id: string;
  thread_id: string;
  from_address: string;
  received_at: string;
}

export interface OutboundAttemptInfo {
  attempt_no: number;
  status: string;
  error_code?: string | null;
  started_at: string;
  finished_at?: string | null;
}

/** Exact frozen snapshot plus dispatch evidence — GET /outbound-messages/{id}. */
export interface OutboundMessageResponse {
  outbound_message_id: string;
  status: string;
  failure_code?: string | null;
  provider_accepted?: boolean | null;
  delivery_status: string;
  thread_id: string | null;
  approval_status: string | null;
  approval_valid: boolean;
  approval_snapshot_matches: boolean;
  approval_expires_at: string | null;
  engagement: OutboundEngagementCounts;
  known_replies: OutboundKnownReply[];
  automatic_resubmit_allowed: boolean;
  next_action: string;
  last_attempt?: OutboundAttemptInfo | null;
  purpose: string;
  from_address: string;
  subject: string;
  body_text: string;
  body_html?: string | null;
  to_address: string;
  payload_hash: string;
  not_before?: string | null;
  accepted_at?: string | null;
  version: number;
  created_at: string;
}

export function messageDraftsPath(options: {
  threadId?: string;
  limit?: number;
  cursor?: string;
} = {}): string {
  const params = new URLSearchParams();
  if (options.threadId) params.set("thread_id", options.threadId);
  params.set("limit", String(options.limit ?? 50));
  if (options.cursor) params.set("cursor", options.cursor);
  return `${MESSAGE_DRAFTS_PATH}?${params.toString()}`;
}

/** The create endpoints use `draft_id`; Inbox's older reply endpoint uses `id`. */
export function messageDraftId(
  draft: { draft_id?: string; id?: string } | null | undefined
): string | undefined {
  return draft?.draft_id ?? draft?.id;
}

export function listMessageDrafts(options: {
  threadId?: string;
  limit?: number;
  cursor?: string;
} = {}): Promise<ApiResult<MessageDraftListItem[]>> {
  return apiFetch<MessageDraftListItem[]>(messageDraftsPath(options));
}

export function createMessageDraft(
  input: CreateMessageDraftInput,
  options?: Pick<MutateOptions, "idempotencyKey">
): Promise<ApiResult<MessageDraftResponse>> {
  return mutate<MessageDraftResponse>(
    MESSAGE_DRAFTS_PATH,
    { method: "POST", json: input },
    options
  );
}

export function getMessageDraft(id: string): Promise<ApiResult<MessageDraftResponse>> {
  return apiFetch<MessageDraftResponse>(
    `${MESSAGE_DRAFTS_PATH}/${encodeURIComponent(id)}`
  );
}

export function freezeMessageDraft(
  id: string,
  input: FreezeMessageDraftInput,
  options: MutateOptions
): Promise<ApiResult<FreezeMessageDraftResult>> {
  return mutate<FreezeMessageDraftResult>(
    `${MESSAGE_DRAFTS_PATH}/${encodeURIComponent(id)}/freeze`,
    { method: "POST", json: input },
    options
  );
}

export function getOutboundMessage(
  id: string
): Promise<ApiResult<OutboundMessageResponse>> {
  return apiFetch<OutboundMessageResponse>(
    `${OUTBOUND_MESSAGES_PATH}/${encodeURIComponent(id)}`
  );
}

/** Non-binding policy report; submit remains separately gated by the caller. */
export function preflightOutbound(
  id: string
): Promise<ApiResult<OutboundPreflightResponse>> {
  return apiFetch<OutboundPreflightResponse>(
    `${OUTBOUND_MESSAGES_PATH}/${encodeURIComponent(id)}/preflight`,
    { method: "POST" }
  );
}

export function requestOutboundApproval(
  id: string,
  input: CreateOutboundApprovalInput,
  options?: Pick<MutateOptions, "idempotencyKey">
): Promise<ApiResult<CreateOutboundApprovalResult>> {
  return mutate<CreateOutboundApprovalResult>(
    `${OUTBOUND_MESSAGES_PATH}/${encodeURIComponent(id)}/approval-requests`,
    { method: "POST", json: input },
    options
  );
}

export function submitOutbound(
  id: string,
  input: SubmitOutboundInput,
  options?: Pick<MutateOptions, "idempotencyKey">
): Promise<ApiResult<SubmitOutboundResult>> {
  return mutate<SubmitOutboundResult>(
    `${OUTBOUND_MESSAGES_PATH}/${encodeURIComponent(id)}/submit`,
    { method: "POST", json: input },
    options
  );
}

export function cancelOutbound(
  id: string,
  input: CancelOutboundInput,
  options: MutateOptions
): Promise<ApiResult<CancelOutboundResult>> {
  return mutate<CancelOutboundResult>(
    `${OUTBOUND_MESSAGES_PATH}/${encodeURIComponent(id)}/cancel`,
    { method: "POST", json: input },
    options
  );
}
