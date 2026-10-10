import { useQuery } from "@tanstack/react-query";

import { apiFetch } from "@/lib/api/client";
import { mutate, type MutateOptions } from "@/lib/api/mutate";

export const SMTP_CONNECTIONS_QUERY_KEY = ["settings", "smtp-connections"] as const;
export const DELIVERY_CHANNELS_QUERY_KEY = [
  "operations",
  "delivery-channels",
] as const;

export type SmtpSecurity = "tls" | "starttls";
export type SmtpConnectionStatus =
  | "pending"
  | "active"
  | "disabled"
  | "reauth_required";

export interface SmtpConnection {
  id: string;
  name: string;
  mail_account_id: string;
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

export interface CreateSmtpConnectionInput {
  name: string;
  mail_account_id: string;
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
