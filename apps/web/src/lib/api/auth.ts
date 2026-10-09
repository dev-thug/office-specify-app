/**
 * 인증 엔드포인트 — openapi.json 생성 전이라 손으로 타이핑한다 (spec §4.5, §4.10).
 * 스키마 생성 후에도 이 래퍼를 유지해 호출부가 경로 문자열에 의존하지 않게 한다.
 *
 * 멱등성: 비-GET 호출은 호출자가 논리 작업당 한 번 발급한 idempotencyKey 를
 * 넘긴다 (폼의 useRef 패턴 — DESIGN.md "API usage" 참조). 생략 시 이 호출마다
 * 새 키를 만들어 쓰므로 재시도 안전망이 필요한 곳에서는 반드시 넘긴다.
 */
import { apiFetch, idempotencyKey, type ApiResult } from "./client";

export type PrincipalKind = "human" | "agent" | "system";
export type Role = "admin" | "manager" | "member" | "agent" | "system" | string;

/** GET /api/v1/me → data 페이로드 (crates/contracts/src/org.rs 의 MeResponse). */
export interface MeMailAccount {
  id: string;
  address: string;
  allowed_actions: string[];
  [key: string]: unknown;
}

export interface MeData {
  principal_id: string;
  kind: PrincipalKind | string;
  role: Role;
  workspace_id: string;
  display_name: string;
  permission_version: number;
  mail_accounts: MeMailAccount[];
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

export interface IdempotentCallOptions {
  /** 논리 작업당 하나 — 호출자가 useRef 등으로 보유한다. */
  idempotencyKey?: string;
}

/** 로그인 — 성공 시 서버가 outreach_session/outreach_csrf 쿠키를 설정한다. */
export async function login(
  input: LoginInput,
  options?: IdempotentCallOptions
): Promise<ApiResult<MeData>> {
  return apiFetch<MeData>("/api/v1/auth/login", {
    method: "POST",
    json: input,
    headers: { "Idempotency-Key": options?.idempotencyKey ?? idempotencyKey() },
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
  input: AcceptInvitationInput,
  options?: IdempotentCallOptions
): Promise<ApiResult<MeData>> {
  return apiFetch<MeData>(
    `/api/v1/auth/invitations/${encodeURIComponent(token)}/accept`,
    {
      method: "POST",
      json: input,
      headers: { "Idempotency-Key": options?.idempotencyKey ?? idempotencyKey() },
    }
  );
}
