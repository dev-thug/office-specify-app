import { useQuery } from "@tanstack/react-query";

import type { DeliveryChannel } from "./settings";
import { apiFetch } from "./client";
import { mutate } from "./mutate";
import type { components } from "./schema";

export type MarketingConnection = components["schemas"]["MarketingConnectionResponse"] & {
  write_scopes_ready?: boolean;
};
export type CampaignsSetup = Omit<components["schemas"]["CampaignsSetupResponse"], "connections"> & {
  connections: MarketingConnection[];
  email_api_configured?: boolean;
};
export type MarketingList = components["schemas"]["MarketingListResponse"];
export type NativeCampaign = components["schemas"]["NativeCampaignResponse"];

export interface MarketingSenderIdentity {
  id: string;
  address: string;
  display_name: string;
  status: string;
}

export interface CampaignsContactField {
  display_name: string;
  field_name: string;
  field_id: string;
}

export type MarketingSuppressionSyncState =
  | "pending"
  | "in_progress"
  | "completed"
  | "unknown"
  | "rejected"
  | "not_connected";

export interface MarketingSuppressionSyncStatus {
  connection_id: string;
  connection_name: string;
  status: string;
  provider_code: string | null;
  updated_at: string | null;
}

export interface CampaignsTopic {
  id: string;
  name: string;
  primary_list: string | null;
}

export interface RegisterMarketingSenderInput {
  mail_account_id: string;
  address: string;
  display_name: string;
  verification_evidence_ref: string;
}

export interface MarketingFieldMapping {
  contact_field: string;
  merge_tag: string;
}

export interface PrepareMarketingExportRequest {
  launch_id: string;
  topic_id?: string;
  field_map: Record<string, MarketingFieldMapping>;
}

export interface MarketingExport {
  id: string;
  launch_id: string;
  connection_id: string;
  state: string;
  version: number;
  source_manifest_hash: string;
  export_hash: string;
  content_hash: string;
  recipient_count: number;
  active_count: number;
  pending_count: number;
  excluded_count: number;
  sender_address: string;
  sender_name: string;
  subject: string;
  provider_list_key: string | null;
  topic_id: string | null;
  provider_campaign_key: string | null;
  last_checked_at: string | null;
  expires_at: string;
  created_at: string;
  report: Record<string, unknown>;
}

export interface MarketingExportContent {
  export_id: string;
  subject: string;
  html: string;
  text: string;
  export_hash: string;
}

export interface SendMarketingExportRequest {
  expected_export_hash: string;
  confirmation: "SEND";
}

export interface CreateMarketingChannelInput {
  connection_id: string;
  mail_account_id: string;
  sender_identity_id: string;
  purpose: "marketing";
  name: string;
}

export function useCampaignsSetup(enabled = true) {
  return useQuery({
    queryKey: ["marketing", "setup"],
    enabled,
    queryFn: () =>
      apiFetch<CampaignsSetup>("/api/v1/integrations/zoho-campaigns"),
  });
}

export function connectCampaigns(key: string) {
  return mutate<components["schemas"]["MarketingConnectResponse"]>(
    "/api/v1/integrations/zoho-campaigns/connect",
    { method: "POST" },
    { idempotencyKey: key }
  );
}

export function getCampaignsLists(id: string) {
  return apiFetch<MarketingList[]>(
    `/api/v1/marketing/connections/${encodeURIComponent(id)}/lists`
  );
}

export function getRecentCampaigns(id: string) {
  return apiFetch<NativeCampaign[]>(
    `/api/v1/marketing/connections/${encodeURIComponent(id)}/campaigns`
  );
}

export function getCampaignsFields(id: string) {
  return apiFetch<CampaignsContactField[]>(
    `/api/v1/marketing/connections/${encodeURIComponent(id)}/fields`
  );
}

export function getMarketingSuppressionSyncStatus(suppressionId: string) {
  return apiFetch<MarketingSuppressionSyncStatus[]>(
    `/api/v1/marketing/suppressions/${encodeURIComponent(suppressionId)}/sync-status`
  );
}

export function getCampaignsTopics(id: string) {
  const query = new URLSearchParams({ from_index: "1", limit: "100" });
  return apiFetch<CampaignsTopic[]>(
    `/api/v1/marketing/connections/${encodeURIComponent(id)}/topics?${query.toString()}`
  );
}

export function disconnectCampaigns(id: string, key: string) {
  return mutate<components["schemas"]["MarketingDisconnectedResponse"]>(
    `/api/v1/marketing/connections/${encodeURIComponent(id)}/disconnect`,
    { method: "POST" },
    { idempotencyKey: key }
  );
}

export function getMarketingSenderIdentities(mailAccountId: string) {
  const query = new URLSearchParams({ mail_account_id: mailAccountId });
  return apiFetch<MarketingSenderIdentity[]>(
    `/api/v1/marketing/sender-identities?${query.toString()}`
  );
}

export function registerMarketingSender(
  input: RegisterMarketingSenderInput,
  key: string
) {
  return mutate<MarketingSenderIdentity>(
    "/api/v1/marketing/sender-identities",
    { method: "POST", json: input },
    { idempotencyKey: key }
  );
}

export function useMarketingSenderIdentities(
  mailAccountId: string | undefined,
  enabled = true
) {
  return useQuery({
    queryKey: ["marketing", "sender-identities", mailAccountId],
    enabled: enabled && Boolean(mailAccountId),
    queryFn: () => getMarketingSenderIdentities(mailAccountId!),
  });
}

/** Uses the shared delivery-channel route; provider derives from connection_id. */
export function createMarketingChannel(input: CreateMarketingChannelInput, key: string) {
  return mutate<DeliveryChannel>(
    "/api/v1/delivery-channels",
    { method: "POST", json: input },
    { idempotencyKey: key }
  );
}

export function prepareMarketingExport(
  input: PrepareMarketingExportRequest,
  key: string
) {
  return mutate<MarketingExport>(
    "/api/v1/marketing/exports",
    { method: "POST", json: input },
    { idempotencyKey: key }
  );
}

export function getMarketingExport(id: string) {
  return apiFetch<MarketingExport>(
    `/api/v1/marketing/exports/${encodeURIComponent(id)}`
  );
}

export function getMarketingExportContent(id: string) {
  return apiFetch<MarketingExportContent>(
    `/api/v1/marketing/exports/${encodeURIComponent(id)}/content`
  );
}

export function syncMarketingExport(id: string, key: string) {
  return mutate<MarketingExport>(
    `/api/v1/marketing/exports/${encodeURIComponent(id)}/sync`,
    { method: "POST" },
    { idempotencyKey: key }
  );
}

export function sendMarketingExport(
  id: string,
  input: SendMarketingExportRequest,
  key: string
) {
  return mutate<MarketingExport>(
    `/api/v1/marketing/exports/${encodeURIComponent(id)}/send`,
    { method: "POST", json: input },
    { idempotencyKey: key }
  );
}

export function refreshMarketingExport(id: string) {
  return apiFetch<MarketingExport>(
    `/api/v1/marketing/exports/${encodeURIComponent(id)}/refresh`,
    { method: "POST" }
  );
}
