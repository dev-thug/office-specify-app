import { useQuery } from "@tanstack/react-query";

import { apiFetch } from "@/lib/api/client";
import { mutate, type MutateOptions } from "@/lib/api/mutate";

export const SMTP_CONNECTIONS_QUERY_KEY = ["settings", "smtp-connections"] as const;
export const SMTP_INBOX_QUERY_KEY = (id: string) =>
  [...SMTP_CONNECTIONS_QUERY_KEY, id, "inbox"] as const;
export const DELIVERY_CHANNELS_QUERY_KEY = [
  "operations",
  "delivery-channels",
] as const;

export type SmtpSecurity = "tls" | "starttls";
export type SmtpPurpose = "marketing" | "correspondence";
export type SmtpConnectionStatus =
  | "pending"
  | "active"
  | "disabled"
  | "reauth_required";

export interface SmtpConnection {
  id: string;
  name: string;
  mail_account_id: string;
  purpose: SmtpPurpose;
  host: string;
  port: number;
  security: SmtpSecurity;
  username: string;
  from_address: string;
  from_name: string;
  reply_to: string | null;
  custom_headers: Record<string, string>;
  status: SmtpConnectionStatus;
  last_error_code: string | null;
  channel_id: string | null;
  sender_identity_id: string | null;
  version: number;
  has_password: boolean;
}

export interface SmtpInboxStatus {
  smtp_connection_id: string;
  mail_account_id: string;
  enabled: boolean;
  imap_host: string;
  imap_port: 993;
  folder: "INBOX";
  cursor_uidvalidity: number | null;
  cursor_uid: number;
  imported_messages: number;
  last_success_at: string | null;
  last_error_code: string | null;
  next_sync_at: string | null;
  active_job_id: string | null;
}

export interface UpdateSmtpInboxSettingsInput {
  enabled: boolean;
  imap_host: string;
}

export interface SmtpInboxSyncResult {
  job_id: string;
  state: "queued";
}

export interface CreateSmtpConnectionInput {
  name: string;
  mail_account_id: string;
  purpose?: SmtpPurpose;
  host: string;
  port: number;
  security: SmtpSecurity;
  username: string;
  password: string;
  from_address: string;
  from_name: string;
  reply_to: string | null;
  custom_headers: Record<string, string>;
  sender_confirmed: true;
}

const SMTP_CONNECTIONS_PATH = "/api/v1/smtp-connections";

export function getSmtpInboxStatus(id: string) {
  return apiFetch<SmtpInboxStatus>(
    `${SMTP_CONNECTIONS_PATH}/${encodeURIComponent(id)}/inbox`
  );
}

export function updateSmtpInboxSettings(
  id: string,
  input: UpdateSmtpInboxSettingsInput,
  options?: MutateOptions
) {
  return mutate<SmtpInboxStatus>(
    `${SMTP_CONNECTIONS_PATH}/${encodeURIComponent(id)}/inbox`,
    { method: "PUT", json: input },
    options
  );
}

export function syncSmtpInbox(id: string, options?: MutateOptions) {
  return mutate<SmtpInboxSyncResult>(
    `${SMTP_CONNECTIONS_PATH}/${encodeURIComponent(id)}/inbox/sync`,
    { method: "POST" },
    options
  );
}

export function useSmtpConnections(enabled = true) {
  return useQuery({
    queryKey: SMTP_CONNECTIONS_QUERY_KEY,
    queryFn: async () => {
      const result = await apiFetch<SmtpConnection[]>(SMTP_CONNECTIONS_PATH);
      return result.data;
    },
    enabled,
  });
}

export function createSmtpConnection(
  input: CreateSmtpConnectionInput,
  options?: MutateOptions
) {
  return mutate<SmtpConnection>(
    SMTP_CONNECTIONS_PATH,
    { method: "POST", json: input },
    options
  );
}

export function verifySmtpConnection(id: string, options?: MutateOptions) {
  return mutate<SmtpConnection>(
    `${SMTP_CONNECTIONS_PATH}/${encodeURIComponent(id)}/verify`,
    { method: "POST" },
    options
  );
}

export function disableSmtpConnection(id: string, options?: MutateOptions) {
  return mutate<SmtpConnection>(
    `${SMTP_CONNECTIONS_PATH}/${encodeURIComponent(id)}/disable`,
    { method: "POST" },
    options
  );
}
