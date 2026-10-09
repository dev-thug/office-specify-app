/**
 * 인증 엔드포인트 — openapi.json 생성 전이라 손으로 타이핑한다 (spec §4.5, §4.10).
 * 스키마 생성 후에도 이 래퍼를 유지해 호출부가 경로 문자열에 의존하지 않게 한다.
 */
import { apiFetch, idempotencyKey, type ApiResult } from "./client";

export type PrincipalKind = "human" | "agent" | "system";
export type Role = "admin" | "manager" | "member" | "agent" | "system" | string;

export interface Principal {
  id: string;
  name: string;
  kind: PrincipalKind | string;
  role: Role;
}

export interface MailboxSummary {
  id: string;
  email?: string;
  display_name?: string;
  [key: string]: unknown;
}

/** GET /api/v1/me → data 페이로드. 추가 필드는 열어 둔다. */
export interface MeData {
  principal: Principal;
  scopes: string[];
  mailboxes: MailboxSummary[];
  allowed_actions: string[];
  [key: string]: unknown;
}

export interface LoginInput {
  email: string;
  password: string;
}

export interface InvitationPreview {
  email: string;
  role: Role;
  expires_at: string;
  workspace_name?: string;
  [key: string]: unknown;
}

export interface AcceptInvitationInput {
  display_name: string;
  password: string;
}

/** 로그인 — 성공 시 서버가 outreach_session/outreach_csrf 쿠키를 설정한다. */
export async function login(input: LoginInput): Promise<ApiResult<MeData>> {
  return apiFetch<MeData>("/api/v1/auth/login", {
    method: "POST",
    json: input,
    headers: { "Idempotency-Key": idempotencyKey() },
  });
}

export async function logout(): Promise<ApiResult<unknown>> {
  return apiFetch("/api/v1/auth/logout", { method: "POST" });
}

export async function getMe(): Promise<ApiResult<MeData>> {
  return apiFetch<MeData>("/api/v1/me");
}

export async function getInvitation(
  token: string
): Promise<ApiResult<InvitationPreview>> {
  return apiFetch<InvitationPreview>(
    `/api/v1/auth/invitations/${encodeURIComponent(token)}`
  );
}

export async function acceptInvitation(
  token: string,
  input: AcceptInvitationInput
): Promise<ApiResult<MeData>> {
  return apiFetch<MeData>(
    `/api/v1/auth/invitations/${encodeURIComponent(token)}/accept`,
    {
      method: "POST",
      json: input,
      headers: { "Idempotency-Key": idempotencyKey() },
    }
  );
}
