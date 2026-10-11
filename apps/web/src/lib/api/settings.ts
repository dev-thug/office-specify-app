/**
 * 설정 API — 조직·연동 표(B.2) 기반 손 타이핑.
 *
 * 부록 B에 명시된 것:
 * - `GET   /principals`                                   — 직원·Agent 상태·역할 목록
 * - `PATCH /principals/{id}`                              — role, status + If-Match
 * - `POST  /principals/{id}/disable-requests`             — 재배정·대기 작업 정책 → 202 job
 * - `POST  /member-invitations`                           — email, role → 201 (일회성 링크 1회 표시, X2)
 * - `POST  /agents`                                       — 이름·허용 scopes → 201
 * - `POST  /agents/{id}/credentials`                      — scopes, expiry → 201 일회성 secret
 * - `POST  /credentials/{id}/revoke`                      — 대기 작업 중단 → 200/202
 * - `GET   /mail-accounts`                                — 접근 가능한 메일함 메타데이터
 * - `POST  /mail-accounts/connection-requests`            — Zoho 지역 → OAuth 시작 정보
 * - `POST  /mail-accounts/{id}/reconnect`                 — 재인증 시작
 * - `GET   /mail-accounts/{id}/grants`                    — grant 목록
 * - `PUT   /mail-accounts/{id}/grants/{principal_id}`     — grant booleans + If-Match (신규는 If-None-Match: *, B.3.2)
 * - `GET/POST /delivery-channels`                         — 목적·허용 상태·capability / 생성
 * - `POST  /delivery-channels/{id}/reviews`               — 증빙·결론 → append-only review
 * - `PATCH /delivery-channels/{id}`                       — enabled·내부 한도 + If-Match
 * - `GET   /calendar-connections`                         — 연결 상태·host별 가시 범위
 * - `POST  /calendar-connections/connection-requests`     — Zoho Calendar OAuth 연결
 * - `POST  /calendar-connections/local`                     — 워크스페이스 내부 일정 준비
 *
 * 부록 B에 없어 화면 구성상 가정한 경로 (계약 확정 시 이 파일만 수정):
 * - `GET /member-invitations`            — 발급한 초대 목록(수락/만료 상태 확인)
 * - `GET /agents/{id}/credentials`       — 에이전트별 credential 메타데이터 목록
 * - `GET /delivery-channels/{id}/reviews`— 채널별 검토 이력
 * - `GET /me/sessions` + `POST /me/sessions/{id}/revoke` — 본인 세션 관리
 * - `PUT /me/password`                   — 본인 비밀번호 변경
 */
import { apiFetch } from "./client";
import { mutate, type MutateOptions } from "./mutate";
import { useCursorList, type ListParams } from "./use-cursor-list";

// ── 주체·초대 ────────────────────────────────────────────────

export type PrincipalKind = "human" | "agent" | "system";
export type PrincipalStatus = "invited" | "active" | "disabled";
/** principals.role — DDL은 human=admin|manager|member, agent=agent, system=system. */
export type WorkspaceRole = "admin" | "manager" | "member" | "agent" | "system";

export interface Principal {
  id: string;
  kind: PrincipalKind | string;
  user_id: string | null;
  name: string;
  display_name?: string | null;
  /** users.email 조인 — 서버가 포함하면 표시, 없으면 생략. */
  email?: string | null;
  role: WorkspaceRole | string;
  status: PrincipalStatus | string;
  permission_version: number;
  version: number;
  created_at: string;
  allowed_actions?: string[];
  [key: string]: unknown;
}

export interface PrincipalFilters {
  kind?: PrincipalKind | "";
  status?: PrincipalStatus | "";
  q?: string;
}

export function usePrincipals(filters?: PrincipalFilters) {
  const params: ListParams = {
    kind: filters?.kind || undefined,
    status: filters?.status || undefined,
    q: filters?.q?.trim() || undefined,
  };
  return useCursorList<Principal>({
    queryKey: ["settings", "principals", params],
    path: "/api/v1/principals",
    params,
  });
}

export interface UpdatePrincipalInput {
  role?: WorkspaceRole;
  status?: PrincipalStatus;
}

/** PATCH /principals/{id} — If-Match 필수 (412 → 최신 값과 비교). */
export function updatePrincipal(
  id: string,
  input: UpdatePrincipalInput,
  options?: MutateOptions
) {
  return mutate<Principal>(
    `/api/v1/principals/${encodeURIComponent(id)}`,
    { method: "PATCH", json: input },
    options
  );
}

export type DisablePendingWorkPolicy = "reassign" | "cancel";

export interface DisableRequestInput {
  /** 대기 작업 처리 정책 — B.2 "재배정·대기 작업 정책" (필드명 가정). */
  pending_work_policy: DisablePendingWorkPolicy;
  /** reassign 정책일 때 업무를 넘길 활성 human principal. */
  reassign_to?: string;
  reason?: string;
}

export interface DisableRequestResult {
  job_id?: string;
  [key: string]: unknown;
}

/** POST /principals/{id}/disable-requests → 202 job. */
export function requestPrincipalDisable(
  id: string,
  input: DisableRequestInput,
  options?: MutateOptions
) {
  return mutate<DisableRequestResult>(
    `/api/v1/principals/${encodeURIComponent(id)}/disable-requests`,
    { method: "POST", json: input },
    options
  );
}

// ── 초대 (member_invitations DDL) ────────────────────────────

export type InvitableRole = "admin" | "manager" | "member";

export interface MemberInvitation {
  id: string;
  email: string;
  role: InvitableRole | string;
  invited_by: string;
  expires_at: string;
  accepted_at: string | null;
  revoked_at: string | null;
  created_at: string;
  [key: string]: unknown;
}

/**
 * 생성 응답 — X2: 일회성 초대 URL을 딱 한 번 반환한다.
 * 필드명은 부록 B에 없어 invitation_url/invite_url/token 순으로 관대하게 읽는다.
 */
export interface MemberInvitationCreated extends MemberInvitation {
  invitation_url?: string;
  invite_url?: string;
  token?: string;
}

/** `GET /api/v1/member-invitations` — 부록 B 미명시 가정. */
export function useMemberInvitations() {
  return useCursorList<MemberInvitation>({
    queryKey: ["settings", "member-invitations"],
    path: "/api/v1/member-invitations",
  });
}

export function createMemberInvitation(
  input: { email: string; role: InvitableRole },
  options?: MutateOptions
) {
  return mutate<MemberInvitationCreated>(
    "/api/v1/member-invitations",
    { method: "POST", json: input },
    options
  );
}

/** 생성 응답에서 복사용 일회성 링크를 해석한다. */
export function invitationLink(created: MemberInvitationCreated): string | undefined {
  return (
    created.invitation_url ??
    created.invite_url ??
    (created.token ? `/invite/${created.token}` : undefined)
  );
}

// ── 에이전트·서비스 credential ───────────────────────────────

export interface CreateAgentInput {
  name: string;
  scopes: string[];
}

export function createAgent(input: CreateAgentInput, options?: MutateOptions) {
  return mutate<Principal>(
    "/api/v1/agents",
    { method: "POST", json: input },
    options
  );
}

/** service_credentials DDL — secret 원문은 저장·표시되지 않는다. */
export interface ServiceCredential {
  id: string;
  principal_id: string;
  name: string;
  key_prefix: string;
  scopes: string[];
  expires_at: string;
  last_used_at: string | null;
  revoked_at: string | null;
  created_at: string;
  [key: string]: unknown;
}

/** 생성 응답 — 일회성 secret을 한 번만 포함한다 (필드명 가정: secret 우선, token 폴백). */
export interface ServiceCredentialCreated extends ServiceCredential {
  secret?: string;
  token?: string;
}

export function credentialSecret(
  created: ServiceCredentialCreated
): string | undefined {
  return created.secret ?? created.token;
}

/** `GET /api/v1/agents/{id}/credentials` — 부록 B 미명시 가정. */
export function useAgentCredentials(agentId: string | undefined) {
  return useCursorList<ServiceCredential>({
    queryKey: ["settings", "agents", agentId, "credentials"],
    path: `/api/v1/agents/${encodeURIComponent(agentId ?? "")}/credentials`,
    enabled: !!agentId,
  });
}

export interface CreateCredentialInput {
  /** 서버에서 필수 — 비워 두면 400. */
  name: string;
  scopes: string[];
  /** 만료까지 일 수 (서버 기본 365, 최대 730). */
  expires_in_days?: number;
  /** true 면 만료 없는 credential — 해지로만 접근을 끝낸다. */
  never_expires?: boolean;
}

export function createAgentCredential(
  agentId: string,
  input: CreateCredentialInput,
  options?: MutateOptions
) {
  return mutate<ServiceCredentialCreated>(
    `/api/v1/agents/${encodeURIComponent(agentId)}/credentials`,
    { method: "POST", json: input },
    options
  );
}

/**
 * POST /credentials/{id}/revoke — 해지 시 대기 작업 중단이 v1.0 기본이다 (§5.5).
 * 대기 작업 유지 여부 필드가 확정되면 입력에 추가한다.
 */
export function revokeCredential(id: string, options?: MutateOptions) {
  return mutate(
    `/api/v1/credentials/${encodeURIComponent(id)}/revoke`,
    { method: "POST" },
    options
  );
}

// ── 메일 계정·grant ──────────────────────────────────────────

/** mail_accounts.status / integration_connections.status 공통 CHECK 집합. */
export type ConnectionStatus = "pending" | "active" | "reauth_required" | "disabled";

export interface MailAccount {
  id: string;
  connection_id: string;
  provider: string;
  provider_account_id: string;
  primary_address: string;
  name: string;
  status: ConnectionStatus | string;
  last_synced_at: string | null;
  last_error_code?: string | null;
  version: number;
  created_at: string;
  [key: string]: unknown;
}

export function useMailAccounts() {
  return useCursorList<MailAccount>({
    queryKey: ["settings", "mail-accounts"],
    path: "/api/v1/mail-accounts",
  });
}

/** OAuth 시작 정보 — 비밀 값이 아닌 브라우저 이동용 URL만 다룬다. */
export interface ConnectionRequestResult {
  authorization_url?: string;
  authorize_url?: string;
  state?: string;
  expires_at?: string;
  [key: string]: unknown;
}

export function authorizationUrl(r: ConnectionRequestResult): string | undefined {
  return r.authorization_url ?? r.authorize_url;
}

export function createMailConnectionRequest(
  input: { region: string; redirect_uri?: string },
  options?: MutateOptions
) {
  return mutate<ConnectionRequestResult>(
    "/api/v1/mail-accounts/connection-requests",
    { method: "POST", json: input },
    options
  );
}

// ── Zoho Mail OAuth 클라이언트 설정 (워크스페이스 저장, secret 쓰기 전용) ──

export interface ZohoOAuthSettings {
  /** client ID·secret·콜백 URI가 모두 유효하면 true. */
  configured: boolean;
  /** `workspace`(설정 화면 저장) | `environment`(서버 env) | `none`. */
  source: "workspace" | "environment" | "none" | string;
  client_id: string | null;
  has_secret: boolean;
  /** 현재 적용 중인 콜백 URI. */
  redirect_uri: string | null;
  /** Zoho API Console에 등록할 기본 콜백 URI. */
  default_redirect_uri: string;
  /** 비어 있거나 잘못된 필드: client_id | client_secret | redirect_uri. */
  missing: string[];
  updated_at: string | null;
}

export interface ZohoOAuthSettingsInput {
  client_id: string;
  /** 생략하면 저장된 secret을 유지한다. */
  client_secret?: string;
  /** 생략하면 기본 콜백 URI를 사용한다. */
  redirect_uri?: string;
}

export const ZOHO_OAUTH_SETTINGS_KEY = ["settings", "zoho-oauth-settings"] as const;

export function getZohoOAuthSettings() {
  return apiFetch<ZohoOAuthSettings>("/api/v1/mail-accounts/oauth-settings");
}

export function putZohoOAuthSettings(
  input: ZohoOAuthSettingsInput,
  options?: MutateOptions
) {
  return mutate<ZohoOAuthSettings>(
    "/api/v1/mail-accounts/oauth-settings",
    { method: "PUT", json: input },
    options
  );
}

export function reconnectMailAccount(id: string, options?: MutateOptions) {
  return mutate<ConnectionRequestResult>(
    `/api/v1/mail-accounts/${encodeURIComponent(id)}/reconnect`,
    { method: "POST" },
    options
  );
}

// ── 메일함 grant (mail_account_grants — §5.4 + B.3.2) ────────

export interface MailAccountGrant {
  mail_account_id: string;
  principal_id: string;
  can_read: boolean;
  can_compose: boolean;
  can_send: boolean;
  can_approve: boolean;
  can_assign: boolean;
  granted_by?: string;
  version: number;
  /** 조인 편의 필드 — 없으면 화면이 principals 목록으로 이름을 해석한다. */
  principal_name?: string;
  principal_kind?: string;
  [key: string]: unknown;
}

export function useMailAccountGrants(mailAccountId: string | undefined) {
  return useCursorList<MailAccountGrant>({
    queryKey: ["settings", "mail-accounts", mailAccountId, "grants"],
    path: `/api/v1/mail-accounts/${encodeURIComponent(mailAccountId ?? "")}/grants`,
    enabled: !!mailAccountId,
  });
}

export interface GrantBooleans {
  can_read: boolean;
  can_compose: boolean;
  can_send: boolean;
  can_approve: boolean;
  can_assign: boolean;
}

/**
 * PUT grants — 기존 행은 If-Match: "<version>", 첫 생성은 If-None-Match: * (B.3.2).
 * Agent can_send is only an ACL prerequisite for human-approved submission. Agents cannot approve or directly submit messages.
 */
export function putMailAccountGrant(
  mailAccountId: string,
  principalId: string,
  grants: GrantBooleans,
  options?: MutateOptions & { create?: boolean }
) {
  const { create, ...rest } = options ?? {};
  return mutate<MailAccountGrant>(
    `/api/v1/mail-accounts/${encodeURIComponent(mailAccountId)}/grants/${encodeURIComponent(principalId)}`,
    { method: "PUT", json: grants },
    {
      ...rest,
      headers: {
        ...(create ? { "If-None-Match": "*" } : {}),
        ...rest.headers,
      },
    }
  );
}

// ── 발송 채널·정책 검토 ──────────────────────────────────────

export type ChannelPurpose = "correspondence" | "marketing" | "transactional";
export type ChannelPolicyStatus = "unverified" | "allowed" | "blocked";
export type DeliveryProvider = "zoho_mail" | "delivery_provider" | "transactional_provider";

export interface DeliveryChannel {
  id: string;
  mail_account_id: string;
  sender_identity_id: string;
  connection_id: string;
  provider: DeliveryProvider | string;
  purpose: ChannelPurpose | string;
  name: string;
  policy_status: ChannelPolicyStatus | string;
  enabled: boolean;
  capabilities: Record<string, unknown>;
  internal_daily_cap: number | null;
  version: number;
  created_at: string;
  [key: string]: unknown;
}

export function useDeliveryChannels() {
  return useCursorList<DeliveryChannel>({
    queryKey: ["operations", "delivery-channels"],
    path: "/api/v1/delivery-channels",
  });
}

export interface CreateDeliveryChannelInput {
  mail_account_id: string;
  sender_identity_id: string;
  connection_id?: string;
  provider: DeliveryProvider;
  purpose: ChannelPurpose;
  name: string;
  internal_daily_cap?: number;
}

export function createDeliveryChannel(
  input: CreateDeliveryChannelInput,
  options?: MutateOptions
) {
  return mutate<DeliveryChannel>(
    "/api/v1/delivery-channels",
    { method: "POST", json: input },
    options
  );
}

/**
 * PATCH /delivery-channels/{id} — 검증된 범위(enabled·내부 한도)만 + If-Match.
 * enabled=true 는 policy_status='allowed' 일 때만 유효 (DDL CHECK — UI도 막는다).
 */
export function patchDeliveryChannel(
  id: string,
  input: { enabled?: boolean; internal_daily_cap?: number | null },
  options?: MutateOptions
) {
  return mutate<DeliveryChannel>(
    `/api/v1/delivery-channels/${encodeURIComponent(id)}`,
    { method: "PATCH", json: input },
    options
  );
}

export type ChannelReviewResult = "allowed" | "blocked" | "unverified";

export interface ChannelReview {
  id: string;
  delivery_channel_id: string;
  reviewed_by: string;
  result: ChannelReviewResult | string;
  purpose: string;
  policy_url: string | null;
  contract_evidence_ref: string | null;
  notes: string | null;
  reviewed_at: string;
  expires_at: string | null;
  created_at: string;
  [key: string]: unknown;
}

/** `GET /api/v1/delivery-channels/{id}/reviews` — 부록 B 미명시 가정. */
export function useChannelReviews(channelId: string | undefined) {
  return useCursorList<ChannelReview>({
    queryKey: ["operations", "delivery-channels", channelId, "reviews"],
    path: `/api/v1/delivery-channels/${encodeURIComponent(channelId ?? "")}/reviews`,
    enabled: !!channelId,
  });
}

export interface CreateChannelReviewInput {
  result: ChannelReviewResult;
  policy_url?: string;
  contract_evidence_ref?: string;
  notes?: string;
  expires_at?: string;
}

export function createChannelReview(
  channelId: string,
  input: CreateChannelReviewInput,
  options?: MutateOptions
) {
  return mutate<ChannelReview>(
    `/api/v1/delivery-channels/${encodeURIComponent(channelId)}/reviews`,
    { method: "POST", json: input },
    options
  );
}

// ── 캘린더 연결 ──────────────────────────────────────────────

export interface CalendarConnection {
  id: string;
  provider: string;
  host_principal_id: string;
  connection_id: string;
  provider_calendar_id: string;
  status: ConnectionStatus | string;
  include_in_busy: boolean;
  version: number;
  created_at: string;
  [key: string]: unknown;
}

export function useCalendarConnections() {
  return useCursorList<CalendarConnection>({
    queryKey: ["settings", "calendar-connections"],
    path: "/api/v1/calendar-connections",
  });
}

export function createCalendarConnectionRequest(
  input: { host_principal_id?: string; region?: string },
  options?: MutateOptions
) {
  return mutate<ConnectionRequestResult>(
    "/api/v1/calendar-connections/connection-requests",
    { method: "POST", json: input },
    options
  );
}


/** Ensures the workspace's Office-managed internal calendar exists. */
export function createLocalCalendarConnection(options?: MutateOptions) {
  return mutate<CalendarConnection>(
    "/api/v1/calendar-connections/local",
    { method: "POST", json: {} },
    options
  );
}

// ── 보안: 본인 세션·비밀번호 (부록 B 미명시 가정 — 상세 참조) ──

export interface UserSession {
  id: string;
  principal_id?: string;
  created_at: string;
  last_seen_at: string | null;
  expires_at: string;
  revoked_at: string | null;
  /** 서버가 현재 세션 표시를 내려주면 배지로 표시한다. */
  current?: boolean;
  [key: string]: unknown;
}

/** `GET /api/v1/me/sessions` — 가정. */
export function useMySessions() {
  return useCursorList<UserSession>({
    queryKey: ["settings", "me", "sessions"],
    path: "/api/v1/me/sessions",
  });
}

/** `POST /api/v1/me/sessions/{id}/revoke` — 가정 (credentials revoke 계약과 동일 패턴). */
export function revokeMySession(id: string, options?: MutateOptions) {
  return mutate(
    `/api/v1/me/sessions/${encodeURIComponent(id)}/revoke`,
    { method: "POST" },
    options
  );
}

export interface ChangePasswordInput {
  current_password: string;
  new_password: string;
}

/** `PUT /api/v1/me/password` — 가정 (§4.5 비밀번호 정책: 12–128자). */
export function changeMyPassword(
  input: ChangePasswordInput,
  options?: MutateOptions
) {
  return mutate(
    "/api/v1/me/password",
    { method: "PUT", json: input },
    options
  );
}
