/**
 * CRM 담당자·주소 연결 API — 부록 B.2/B.3.4 + contacts/email_addresses/
 * contact_emails DDL CHECK 값 기준 수작업 타입.
 *
 * 안전 규칙 (§7.3 UI04, §6.2):
 * - 주소의 "형식·도달 검증"(verification_status)과 "목적별 발송 허가"
 *   (marketing_eligibility)는 다른 축이다 — UI에서 별도 배지로 표시한다.
 * - 수신 거부·차단 주소에는 suppression 표시를 붙이고 발송 유도를 하지 않는다.
 * - 형식이 맞는 주소를 만들어도 verified/consented 로 저장하지 않는다 (B.3.4).
 */
import { apiFetch, type ApiResult } from "./client";
import { mutate } from "./mutate";
import type { ListParams } from "./use-cursor-list";

export const CONTACTS_PATH = "/api/v1/contacts";

// ── 담당자 ────────────────────────────────────────────────────

/** contacts.status CHECK 값. */
export type ContactStatus = "new" | "active" | "departed" | "inactive";

export const CONTACT_STATUSES: readonly ContactStatus[] = [
  "new",
  "active",
  "departed",
  "inactive",
];

/** email_addresses.verification_status CHECK 값 — 형식/도달 검증 축. */
export type VerificationStatus = "unknown" | "verified" | "invalid" | "risky";

/** 목적별 발송 가능성 — B.3.4 marketing_eligibility. */
export interface SendEligibility {
  eligible: boolean;
  /** B.5·정책 코드 — 예: SEND_BASIS_UNVERIFIED, WORKSPACE_MARKETING_SUPPRESSION. */
  reason_codes: string[];
}

/** 주소에 걸린 차단(수신 거부) 참조 — 응답에 포함될 때만 표시한다. */
export interface SuppressionRef {
  id?: string;
  purpose_scope?: string;
  reason?: string;
  [key: string]: unknown;
}

/** contact_emails 연결 + email_addresses 상태의 표시 모델. */
export interface ContactEmail {
  email_address_id: string;
  address: string;
  label?: string | null;
  is_primary: boolean;
  verification_status: VerificationStatus | string;
  marketing_eligibility?: SendEligibility;
  linked_contact_ids?: string[];
  /** 서버가 주는 경우의 차단 목록. 없으면 reason_codes 로 추정한다. */
  suppressions?: SuppressionRef[];
  [key: string]: unknown;
}

export interface Contact {
  id: string;
  company_id: string | null;
  /** 목록·상세에서 서버가 조인해 줄 수 있는 회사명. */
  company_name?: string | null;
  name: string;
  job_title: string | null;
  department: string | null;
  owner_principal_id: string | null;
  owner_display_name?: string | null;
  status: ContactStatus | string;
  archived_at: string | null;
  /** 상세에서 연결된 주소 목록 (UI04) — 목록 응답에는 없을 수 있다. */
  emails?: ContactEmail[];
  version: number;
  updated_at: string;
  created_at: string;
  allowed_actions?: string[];
  [key: string]: unknown;
}

export interface CreateContactInput {
  name: string;
  company_id?: string;
  job_title?: string;
  department?: string;
  owner_principal_id?: string;
}

/** PATCH /contacts/{id} — allowlist 필드만 (B.1.1). */
export interface PatchContactInput {
  name?: string;
  company_id?: string | null;
  job_title?: string | null;
  department?: string | null;
  status?: ContactStatus | string;
}

// ── 주소 연결 ────────────────────────────────────────────────

/** POST /contacts/{id}/emails 본문 (B.3.4). */
export interface AddContactEmailInput {
  address: string;
  label?: string;
  is_primary?: boolean;
}

export interface AddContactEmailResult {
  email_address_id: string;
  address: string;
  verification_status: VerificationStatus | string;
  marketing_eligibility: SendEligibility;
  linked_contact_ids?: string[];
  /** 이미 다른 담당자와 연결된 주소일 때 서버가 설명을 실어 줄 수 있다. */
  conflict?: { contact_id?: string; message?: string } | null;
  [key: string]: unknown;
}

/** PUT /contacts/{id}/primary-email 본문. */
export interface SetPrimaryEmailInput {
  email_address_id: string;
}

// ── 차단 판정 헬퍼 ───────────────────────────────────────────

/** reason_codes·suppressions 로 차단 여부를 보수적으로 판정한다. */
export function isSuppressed(email: ContactEmail): boolean {
  if (email.suppressions && email.suppressions.length > 0) return true;
  const codes = email.marketing_eligibility?.reason_codes ?? [];
  return codes.some(
    (c) => c.includes("SUPPRESSION") || c === "RECIPIENT_SUPPRESSED"
  );
}

// ── 함수 ────────────────────────────────────────────────────

export function contactsPath(): string {
  return CONTACTS_PATH;
}

export interface ContactListFilter {
  q?: string;
  status?: string;
  company_id?: string;
  owner_principal_id?: string;
}

export function contactListParams(f: ContactListFilter): ListParams {
  return {
    q: f.q || undefined,
    status: f.status || undefined,
    company_id: f.company_id || undefined,
    owner_principal_id: f.owner_principal_id || undefined,
  };
}

export async function getContact(id: string): Promise<ApiResult<Contact>> {
  return apiFetch<Contact>(`${CONTACTS_PATH}/${encodeURIComponent(id)}`);
}

export async function createContact(
  input: CreateContactInput,
  options?: { idempotencyKey?: string }
): Promise<ApiResult<Contact>> {
  return mutate<Contact>(
    CONTACTS_PATH,
    { method: "POST", json: input },
    { idempotencyKey: options?.idempotencyKey }
  );
}

export async function patchContact(
  id: string,
  input: PatchContactInput,
  options: { etag: string | number; idempotencyKey?: string }
): Promise<ApiResult<Contact>> {
  return mutate<Contact>(
    `${CONTACTS_PATH}/${encodeURIComponent(id)}`,
    { method: "PATCH", json: input },
    { etag: options.etag, idempotencyKey: options.idempotencyKey }
  );
}

export async function addContactEmail(
  contactId: string,
  input: AddContactEmailInput,
  options?: { idempotencyKey?: string }
): Promise<ApiResult<AddContactEmailResult>> {
  return mutate<AddContactEmailResult>(
    `${CONTACTS_PATH}/${encodeURIComponent(contactId)}/emails`,
    { method: "POST", json: input },
    { idempotencyKey: options?.idempotencyKey }
  );
}

export async function setPrimaryEmail(
  contactId: string,
  input: SetPrimaryEmailInput,
  options?: { etag?: string | number; idempotencyKey?: string }
): Promise<ApiResult<Contact>> {
  return mutate<Contact>(
    `${CONTACTS_PATH}/${encodeURIComponent(contactId)}/primary-email`,
    { method: "PUT", json: input },
    { etag: options?.etag, idempotencyKey: options?.idempotencyKey }
  );
}
