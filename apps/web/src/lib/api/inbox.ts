/**
 * inbox API — 공동 Inbox (source §9, B.2 `email-threads` 행, B.3.10).
 *
 * DTO 는 부록 B 필드명 그대로 snake_case 유지. openapi.json 생성 전이라 손으로
 * 타이핑한다 (DESIGN.md "DTO 타이핑"). 상태 유니온은 부록 A DDL CHECK 값이다.
 *
 * 멱등성·낙관적 잠금:
 * - assignment / PATCH status 는 If-Match 필수 (B.1.4) → etag 를 넘긴다.
 * - read-state PUT 은 단조 증가 갱신이라 If-Match 없이 처리된다 (B.1.4).
 * - 비-GET 은 호출자가 보유한 논리 작업당 Idempotency-Key 를 싣는다.
 */
import { useQuery } from "@tanstack/react-query";

import { apiFetch, type ApiResult } from "./client";
import { mutate } from "./mutate";
import { useCursorList, type ListParams } from "./use-cursor-list";

const API = "/api/v1";

// ─── 상태 유니온 (DDL CHECK) ────────────────────────────────

export type ThreadStatus = "open" | "pending" | "resolved";
export const THREAD_STATUSES: readonly ThreadStatus[] = [
  "open",
  "pending",
  "resolved",
];

export type MessageDirection = "inbound" | "outbound";

export type MessageClassification =
  | "human_positive"
  | "human_negative"
  | "question"
  | "unsubscribe_request"
  | "auto_reply"
  | "bounce"
  | "unknown";
export const MESSAGE_CLASSIFICATIONS: readonly MessageClassification[] = [
  "human_positive",
  "human_negative",
  "question",
  "unsubscribe_request",
  "auto_reply",
  "bounce",
  "unknown",
];

export type MailAccountStatus =
  | "pending"
  | "active"
  | "reauth_required"
  | "disabled";

export type ParticipantRole = "from" | "to" | "cc" | "bcc" | "reply_to";

export type DraftVisibility = "private" | "mailbox";

// ─── DTO ──────────────────────────────────────────────────

/** GET /email-threads 목록 행 — 정렬은 (updated_at DESC, id DESC) (B.1.5). */
export interface EmailThreadSummary {
  id: string;
  mail_account_id: string;
  contact_id: string | null;
  assigned_principal_id: string | null;
  assigned_principal_name?: string | null;
  contact_name?: string | null;
  company_name?: string | null;
  subject?: string | null;
  snippet?: string | null;
  status: ThreadStatus;
  last_inbound_message_id: string | null;
  last_message_at?: string | null;
  /** 목록 응답이 제공하면 그대로 쓰는 본인 기준 안읽음 플래그. */
  unread?: boolean;
  /** unread 가 없을 때 비교하는 폴백 시각들. */
  last_inbound_at?: string | null;
  last_read_at?: string | null;
  version: number;
  updated_at: string;
  created_at: string;
  [key: string]: unknown;
}

export interface MessageParticipant {
  role: ParticipantRole;
  address: string;
  display_name: string | null;
  email_address_id?: string | null;
}

export interface MessageAttachment {
  id: string;
  filename: string;
  is_inline: boolean;
  size_bytes?: number | null;
  content_type?: string | null;
  [key: string]: unknown;
}

export interface EmailMessage {
  id: string;
  thread_id?: string;
  direction: MessageDirection;
  /** 공급자 ID 는 문자열 그대로 — 숫자 변환 금지 (§9.2). */
  provider_message_id?: string | null;
  subject: string | null;
  body_text: string | null;
  /**
   * 서버(ammonia)가 정제한 HTML 만 노출된다 (§15.1). 클라이언트는 이 필드를
   * sandboxed iframe srcDoc + CSP meta 로만 렌더하고, 없으면 body_text 를
   * 텍스트로 보여준다 — 정제되지 않은 HTML 을 주입하지 않는다.
   */
  body_html_sanitized: string | null;
  message_at: string;
  observed_at?: string;
  classification: MessageClassification;
  classification_reviewed_by?: string | null;
  participants?: MessageParticipant[];
  attachments?: MessageAttachment[];
  /** 연결된 발송 작업의 업무 상태 — accepted 는 전달 완료가 아니다 (B.3.11). */
  outbound_status?: string | null;
  [key: string]: unknown;
}

export interface ThreadCrmEmailAddress {
  id?: string;
  address: string;
  is_primary?: boolean;
  /** 이메일 형식 검증 — 발송 허가와는 다른 개념 (UI04 규칙). */
  is_verified?: boolean;
  /** 발송 허가(근거+차단 아님) — 별도 배지로 표시한다. */
  send_permitted?: boolean;
  suppressed?: boolean;
  [key: string]: unknown;
}

export interface ThreadCrmContact {
  id: string;
  name: string;
  title?: string | null;
  company_id?: string | null;
  company_name?: string | null;
  email_addresses?: ThreadCrmEmailAddress[];
  [key: string]: unknown;
}

export interface ThreadCrmCompany {
  id: string;
  name: string;
  domain?: string | null;
  lifecycle_stage?: string | null;
  [key: string]: unknown;
}

/** GET /email-threads/{id} 의 crm 보조 패널 페이로드. */
export interface ThreadCrmContext {
  contact?: ThreadCrmContact | null;
  company?: ThreadCrmCompany | null;
  [key: string]: unknown;
}

export interface ThreadMailAccount {
  id: string;
  name?: string;
  primary_address?: string;
  status?: MailAccountStatus;
  [key: string]: unknown;
}

/** GET /email-threads/{id} → data — 메시지·CRM·스레드 버전 (B.2). */
export interface EmailThread {
  id: string;
  mail_account_id: string;
  contact_id: string | null;
  assigned_principal_id: string | null;
  assigned_principal_name?: string | null;
  last_inbound_message_id: string | null;
  status: ThreadStatus;
  subject?: string | null;
  version: number;
  updated_at: string;
  created_at: string;
  [key: string]: unknown;
}

export interface EmailThreadDetailData {
  thread: EmailThread;
  messages: EmailMessage[];
  crm?: ThreadCrmContext | null;
  mail_account?: ThreadMailAccount | null;
  /** UI 힌트 — 서버가 최종 검사한다 (§12.2). */
  allowed_actions?: string[];
  [key: string]: unknown;
}

/** GET /mail-accounts 행 — 접근 가능한 메일함 메타데이터. */
export interface MailAccountGrant {
  can_read?: boolean;
  can_compose?: boolean;
  can_send?: boolean;
  can_approve?: boolean;
  can_assign?: boolean;
}

export interface MailAccount {
  id: string;
  name: string;
  primary_address: string;
  status: MailAccountStatus;
  last_synced_at?: string | null;
  grant?: MailAccountGrant | null;
  allowed_actions?: string[];
  [key: string]: unknown;
}

/** GET /principals 행 — 배정 대상 피커용 요약. */
export interface PrincipalSummary {
  id: string;
  kind: "human" | "agent" | "system" | string;
  name: string;
  display_name?: string | null;
  role: string;
  status: "invited" | "active" | "disabled" | string;
  [key: string]: unknown;
}

// ─── 요청·응답 DTO ─────────────────────────────────────────

/** PUT /email-threads/{id}/assignment — null 이면 배정 해제. */
export interface AssignThreadInput {
  assigned_principal_id: string | null;
}

/** PUT /email-threads/{id}/read-state — 본인의 읽음 위치만 갱신한다. */
export interface ReadStateInput {
  last_read_message_id: string;
}

/** PATCH /email-threads/{id} — 문서화된 패치 필드는 status 뿐이다. */
export interface PatchThreadInput {
  status: ThreadStatus;
}

/** POST /email-threads/{id}/reply-drafts 요청 (B.3.10 필드 그대로). */
export interface ReplyDraftInput {
  expected_last_inbound_id: string;
  body_text: string;
  body_html?: string | null;
  visibility?: DraftVisibility;
}

export interface ReplyDraftResult {
  id: string;
  status?: string;
  thread_id?: string;
  /** 서버가 확인한 실제 수신 주소 — 사용자에게 그대로 보여준다 (B.3.10). */
  to_address?: string | null;
  to_email_address_id?: string | null;
  [key: string]: unknown;
}

/** POST /email-messages/{id}/classification-reviews 요청. */
export interface ClassificationReviewInput {
  classification: MessageClassification;
  reason?: string;
}

export interface ClassificationReviewResult {
  id?: string;
  classification?: MessageClassification;
  [key: string]: unknown;
}

/** POST /message-attachments/{id}/download-requests → 짧은 수명 URL. */
export interface AttachmentDownloadResult {
  download_url: string;
  expires_at?: string;
  [key: string]: unknown;
}

// ─── 조회 ─────────────────────────────────────────────────

/** 목록 필터 — undefined/null 은 쿼리스트링에서 제외한다. */
export interface EmailThreadFilters {
  mail_account_id?: string;
  /** 배정 필터 — "me" | "unassigned" (서버 해석 토큰). */
  assigned?: "me" | "unassigned";
  status?: ThreadStatus;
  q?: string;
}

export function useEmailThreads(filters: EmailThreadFilters) {
  const params: ListParams = {
    mail_account_id: filters.mail_account_id,
    assigned: filters.assigned,
    status: filters.status,
    q: filters.q,
  };
  return useCursorList<EmailThreadSummary>({
    queryKey: ["inbox", "threads", params],
    path: `${API}/email-threads`,
    params,
  });
}

export function getEmailThread(
  threadId: string
): Promise<ApiResult<EmailThreadDetailData>> {
  return apiFetch<EmailThreadDetailData>(
    `${API}/email-threads/${encodeURIComponent(threadId)}`
  );
}

export function useEmailThread(
  threadId: string,
  options?: { refetchIntervalMs?: number }
) {
  return useQuery({
    queryKey: ["inbox", "thread", threadId],
    enabled: threadId.length > 0,
    refetchInterval: options?.refetchIntervalMs ?? false,
    queryFn: () => getEmailThread(threadId),
  });
}

export function useMailAccounts() {
  return useQuery({
    queryKey: ["inbox", "mail-accounts"],
    staleTime: 30_000,
    queryFn: () => apiFetch<MailAccount[]>(`${API}/mail-accounts`),
  });
}

export function usePrincipals() {
  return useQuery({
    queryKey: ["inbox", "principals"],
    staleTime: 60_000,
    queryFn: () => apiFetch<PrincipalSummary[]>(`${API}/principals`),
  });
}

// ─── 변경 ─────────────────────────────────────────────────

export interface ThreadMutationOptions {
  /** 저장해 둔 리소스 ETag/버전 — If-Match 로 전송한다. */
  etag?: string | number;
  /** 논리 작업당 하나 — 호출자가 useRef 로 보유한다. */
  idempotencyKey?: string;
}

export function putAssignment(
  threadId: string,
  input: AssignThreadInput,
  options?: ThreadMutationOptions
) {
  return mutate<EmailThread>(
    `${API}/email-threads/${encodeURIComponent(threadId)}/assignment`,
    { method: "PUT", json: input },
    { etag: options?.etag, idempotencyKey: options?.idempotencyKey }
  );
}

/** 본인 읽음 위치 갱신 — 단조 증가 PUT 이라 If-Match 를 보내지 않는다 (B.1.4). */
export function putReadState(
  threadId: string,
  input: ReadStateInput,
  options?: Pick<ThreadMutationOptions, "idempotencyKey">
) {
  return mutate(
    `${API}/email-threads/${encodeURIComponent(threadId)}/read-state`,
    { method: "PUT", json: input },
    { idempotencyKey: options?.idempotencyKey }
  );
}

export function patchThreadStatus(
  threadId: string,
  input: PatchThreadInput,
  options?: ThreadMutationOptions
) {
  return mutate<EmailThread>(
    `${API}/email-threads/${encodeURIComponent(threadId)}`,
    { method: "PATCH", json: input },
    { etag: options?.etag, idempotencyKey: options?.idempotencyKey }
  );
}

export function createReplyDraft(
  threadId: string,
  input: ReplyDraftInput,
  options?: Pick<ThreadMutationOptions, "idempotencyKey">
) {
  return mutate<ReplyDraftResult>(
    `${API}/email-threads/${encodeURIComponent(threadId)}/reply-drafts`,
    { method: "POST", json: input },
    { idempotencyKey: options?.idempotencyKey }
  );
}

export function createClassificationReview(
  messageId: string,
  input: ClassificationReviewInput,
  options?: Pick<ThreadMutationOptions, "idempotencyKey">
) {
  return mutate<ClassificationReviewResult>(
    `${API}/email-messages/${encodeURIComponent(messageId)}/classification-reviews`,
    { method: "POST", json: input },
    { idempotencyKey: options?.idempotencyKey }
  );
}

export function requestAttachmentDownload(
  attachmentId: string,
  options?: Pick<ThreadMutationOptions, "idempotencyKey">
) {
  return mutate<AttachmentDownloadResult>(
    `${API}/message-attachments/${encodeURIComponent(attachmentId)}/download-requests`,
    { method: "POST", json: {} },
    { idempotencyKey: options?.idempotencyKey }
  );
}
