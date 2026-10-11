/**
 * CRM 회사·활동·메모·업무 API — 부록 B.2 CRM 행 + DDL CHECK 값 기준 수작업 타입.
 * (spec §12.4: openapi 스키마 생성 전까지 이 래퍼가 호출부의 계약이다.
 *  스키마 생성 후 `paths` 타입으로 갈아끼우되 호출부는 이 래퍼를 유지한다.)
 *
 * 소스 근거:
 * - companies/contacts/notes/tasks/activities DDL (A.4), 상태 CHECK (§6.2)
 * - POST /companies → 201 + duplicate_candidates (B.3.3)
 * - POST /companies/{id}/merge-requests → 202 job (B.2, integration-contracts `crm.merge_companies`)
 * - GET /companies/{id}/activities — 허용 활동만 cursor (B.2, §5.6)
 * - POST /notes 대상 1개·본문 → 201 (B.2)
 * - GET/POST /tasks, PATCH /tasks/{id} + If-Match (B.2, B.1.4)
 * - POST /exports → 202 export job (B.2)
 */
import { apiFetch, type ApiResult } from "./client";
import { mutate } from "./mutate";
import type { ListParams } from "./use-cursor-list";

export const COMPANIES_PATH = "/api/v1/companies";
export const TASKS_PATH = "/api/v1/tasks";

// ── 회사 ────────────────────────────────────────────────────

/** companies.lifecycle_stage CHECK 값 (A.4). */
export type LifecycleStage =
  | "prospect"
  | "engaged"
  | "qualified"
  | "customer"
  | "inactive";

export const LIFECYCLE_STAGES: readonly LifecycleStage[] = [
  "prospect",
  "engaged",
  "qualified",
  "customer",
  "inactive",
];

export interface Company {
  id: string;
  name: string;
  domain: string | null;
  website_url: string | null;
  industry: string | null;
  /** ISO 3166-1 alpha-2 — DDL CHECK `^[A-Z]{2}$`. */
  country_code: string | null;
  lifecycle_stage: LifecycleStage | string;
  owner_principal_id: string | null;
  /** 서버가 조인해 줄 수 있는 표시용 이름 — 없으면 principals 목록으로 해석한다. */
  owner_display_name?: string | null;
  description: string | null;
  custom_fields: Record<string, unknown>;
  archived_at: string | null;
  version: number;
  updated_at: string;
  created_at: string;
  /** 목록 행의 "최근 허용 활동" 표시용 (UI02) — ACL 통과분만 서버가 채운다. */
  last_activity_at?: string | null;
  last_activity_summary?: string | null;
  /** 화면 힌트 — 서버 검사를 대신하지 않는다 (§12.2). */
  allowed_actions?: string[];
  [key: string]: unknown;
}

/** POST /companies 응답 data — 같은 도메인 후보 (B.3.3). */
export interface CompanyDuplicateCandidate {
  id: string;
  name: string;
  domain?: string | null;
  match_reason?: string;
  [key: string]: unknown;
}

export interface CreateCompanyInput {
  name: string;
  domain?: string;
  website_url?: string;
  industry?: string;
  country_code?: string;
  /** Member가 타인을 지정하면 서버가 거절한다 (B.3.3). */
  owner_principal_id?: string;
  description?: string;
}

export interface CompanyCreated extends Company {
  duplicate_candidates: CompanyDuplicateCandidate[];
}

/** PATCH /companies/{id} — 문서화된 allowlist 필드만 (B.1.1, B.2). */
export interface PatchCompanyInput {
  name?: string;
  domain?: string | null;
  website_url?: string | null;
  industry?: string | null;
  country_code?: string | null;
  lifecycle_stage?: LifecycleStage | string;
  description?: string | null;
}

/** POST /companies/{id}/archive 본문 — 관련 작업 검토 메모 (B.2). */
export interface ArchiveCompanyInput {
  reason?: string;
}

/** POST /companies/{id}/merge-requests body; path id identifies the surviving target. */
export interface MergeRequestInput {
  source_id: string;
  field_choices?: Record<string, "source" | "target">;
}

export interface MergeRequestAccepted {
  job_id?: string;
  merge_request_id?: string;
  status?: string;
  [key: string]: unknown;
}

// ── 활동 타임라인 (§7.4 Activity/Emails/Meetings 탭 공통 원천) ──

/** activities DDL + summary는 비민감 라벨만 담는다 (integration-contracts). */
export interface CompanyActivity {
  id: string;
  company_id: string | null;
  contact_id: string | null;
  mail_account_id: string | null;
  thread_id: string | null;
  booking_id: string | null;
  actor_principal_id: string | null;
  actor_display_name?: string | null;
  kind: string;
  source_event_id: string;
  occurred_at: string;
  summary: Record<string, unknown>;
  [key: string]: unknown;
}

/** 메일 행 — mail_account_id/thread_id 가 있는 활동. 읽기 grant 없으면 서버가 행 자체를 내주지 않는다(§5.6). */
export function isMailActivity(a: CompanyActivity): boolean {
  return a.thread_id != null || a.mail_account_id != null;
}

export function isBookingActivity(a: CompanyActivity): boolean {
  return a.booking_id != null || a.kind.startsWith("booking");
}

export function isNoteActivity(a: CompanyActivity): boolean {
  return a.kind.startsWith("note");
}

export function isTaskActivity(a: CompanyActivity): boolean {
  return a.kind.startsWith("task");
}

export function isSuppressionActivity(a: CompanyActivity): boolean {
  return a.kind.startsWith("suppression");
}

/** 리서치·리드 증빙 계열 행 — Research 탭·우측 rail의 "검토 필요" 카드 원천. */
export function isResearchActivity(a: CompanyActivity): boolean {
  return a.kind.startsWith("research") || a.kind.startsWith("lead");
}

// ── 메모·업무 (CRM 공용 — contacts 기능도 같은 엔드포인트를 쓴다) ──

export interface Note {
  id: string;
  company_id: string | null;
  contact_id: string | null;
  thread_id: string | null;
  mail_account_id: string | null;
  created_by: string;
  body: string;
  version: number;
  updated_at: string;
  created_at: string;
  [key: string]: unknown;
}

/** POST /notes — 대상은 정확히 1개 (DDL num_nonnulls CHECK). */
export interface CreateNoteInput {
  company_id?: string;
  contact_id?: string;
  body: string;
}

/** tasks.status CHECK 값. */
export type TaskStatus = "open" | "done" | "canceled";

export interface TaskItem {
  id: string;
  title: string;
  owner_principal_id: string;
  owner_display_name?: string | null;
  company_id: string | null;
  contact_id: string | null;
  thread_id: string | null;
  status: TaskStatus | string;
  due_at: string | null;
  version: number;
  created_at: string;
  allowed_actions?: string[];
  [key: string]: unknown;
}

export interface CreateTaskInput {
  title: string;
  owner_principal_id?: string;
  company_id?: string;
  contact_id?: string;
  /** RFC 3339. */
  due_at?: string;
}

export interface PatchTaskInput {
  title?: string;
  owner_principal_id?: string;
  status?: TaskStatus | string;
  due_at?: string | null;
}

// ── 조직 읽기 (owner 선택·표시명 해석용) ─────────────────────

export interface Principal {
  id: string;
  kind: "human" | "agent" | "system" | string;
  role?: string;
  /** Some API versions return `name`; display_name is the UI-oriented alias. */
  name?: string | null;
  display_name?: string | null;
  status?: string;
  [key: string]: unknown;
}

// ── CSV 보내기 (B.2 POST /exports → 202) ─────────────────────

export interface CrmExportInput {
  /** Exportable field allowlist from `domain::imports::EXPORTABLE_FIELDS`. */
  fields: string[];
  filters?: {
    company_ids?: string[];
    lifecycle_stage?: string;
    q?: string;
  };
}

export interface ExportAccepted {
  import_id?: string;
  job_id?: string;
  export_id?: string;
  status?: string;
  [key: string]: unknown;
}

// ── allowed_actions 힌트 (§12.2 — 화면 표시용, 서버가 재검사) ──

/**
 * 부록 B는 action 문자열의 정확한 값을 정하지 않았다. 가능한 이름들을
 * 묶어 검사하고, `allowed_actions` 필드 자체가 없으면 노출하되 서버 403에
 * 의존한다 (보고서 "assumptions" 참조).
 */
export function allowsAction(
  allowed: string[] | undefined,
  ...aliases: string[]
): boolean {
  if (allowed === undefined) return true;
  return aliases.some((a) => allowed.includes(a));
}

export const ACTION_UPDATE = ["update", "patch", "edit"];
export const ACTION_ARCHIVE = ["archive"];
export const ACTION_MERGE = ["merge", "merge_request", "request_merge"];
export const ACTION_ADD_CONTACT = ["add_contact", "create_contact"];
export const ACTION_ADD_NOTE = ["add_note", "create_note", "note"];
export const ACTION_ADD_TASK = ["add_task", "create_task", "task"];
export const ACTION_ADD_EMAIL = ["add_email", "create_email"];
export const ACTION_SET_PRIMARY = ["set_primary_email", "update_emails"];
export const ACTION_EXPORT = ["export", "crm.export"];
export const ACTION_RESEARCH = ["request_research", "create_agent_run"];

// ── 함수 ────────────────────────────────────────────────────

/** 회사 목록 커서 경로 — useCursorList 와 함께 쓴다. */
export function companiesPath(): string {
  return COMPANIES_PATH;
}

export function companyActivitiesPath(companyId: string): string {
  return `${COMPANIES_PATH}/${encodeURIComponent(companyId)}/activities`;
}

export function tasksPath(params?: {
  companyId?: string;
  contactId?: string;
  status?: string;
}): string {
  const qs = new URLSearchParams();
  if (params?.companyId) qs.set("company_id", params.companyId);
  if (params?.contactId) qs.set("contact_id", params.contactId);
  if (params?.status) qs.set("status", params.status);
  const s = qs.toString();
  return s ? `${TASKS_PATH}?${s}` : TASKS_PATH;
}

export async function getCompany(id: string): Promise<ApiResult<Company>> {
  return apiFetch<Company>(`${COMPANIES_PATH}/${encodeURIComponent(id)}`);
}

export async function createCompany(
  input: CreateCompanyInput,
  options?: { idempotencyKey?: string }
): Promise<ApiResult<CompanyCreated>> {
  return mutate<CompanyCreated>(COMPANIES_PATH, {
    method: "POST",
    json: input,
  }, { idempotencyKey: options?.idempotencyKey });
}

export async function patchCompany(
  id: string,
  input: PatchCompanyInput,
  options: { etag: string | number; idempotencyKey?: string }
): Promise<ApiResult<Company>> {
  return mutate<Company>(
    `${COMPANIES_PATH}/${encodeURIComponent(id)}`,
    { method: "PATCH", json: input },
    { etag: options.etag, idempotencyKey: options.idempotencyKey }
  );
}

export async function archiveCompany(
  id: string,
  input: ArchiveCompanyInput,
  options?: { etag?: string | number; idempotencyKey?: string }
): Promise<ApiResult<Company>> {
  return mutate<Company>(
    `${COMPANIES_PATH}/${encodeURIComponent(id)}/archive`,
    { method: "POST", json: input },
    { etag: options?.etag, idempotencyKey: options?.idempotencyKey }
  );
}

export async function createCompanyMergeRequest(
  targetCompanyId: string,
  input: MergeRequestInput,
  options?: { idempotencyKey?: string }
): Promise<ApiResult<MergeRequestAccepted>> {
  return mutate<MergeRequestAccepted>(
    `${COMPANIES_PATH}/${encodeURIComponent(targetCompanyId)}/merge-requests`,
    {
      method: "POST",
      json: {
        source_id: input.source_id,
        field_choices: input.field_choices,
      },
    },
    { idempotencyKey: options?.idempotencyKey }
  );
}

export async function createNote(
  input: CreateNoteInput,
  options?: { idempotencyKey?: string }
): Promise<ApiResult<Note>> {
  return mutate<Note>(
    "/api/v1/notes",
    { method: "POST", json: input },
    { idempotencyKey: options?.idempotencyKey }
  );
}

export async function createTask(
  input: CreateTaskInput,
  options?: { idempotencyKey?: string }
): Promise<ApiResult<TaskItem>> {
  return mutate<TaskItem>(
    TASKS_PATH,
    { method: "POST", json: input },
    { idempotencyKey: options?.idempotencyKey }
  );
}

export async function patchTask(
  id: string,
  input: PatchTaskInput,
  options: { etag: string | number; idempotencyKey?: string }
): Promise<ApiResult<TaskItem>> {
  return mutate<TaskItem>(
    `${TASKS_PATH}/${encodeURIComponent(id)}`,
    { method: "PATCH", json: input },
    { etag: options.etag, idempotencyKey: options.idempotencyKey }
  );
}

/** owner 표시명·지정용 주체 목록 — GET /principals (조직 조회). */
export async function listPrincipals(): Promise<ApiResult<Principal[]>> {
  const result = await apiFetch<Principal[]>("/api/v1/principals");
  return {
    ...result,
    data: result.data.map((principal) => ({
      ...principal,
      display_name:
        principal.display_name?.trim() || principal.name?.trim() || undefined,
    })),
  };
}

export async function requestCrmExport(
  input: CrmExportInput,
  options?: { idempotencyKey?: string }
): Promise<ApiResult<ExportAccepted>> {
  return mutate<ExportAccepted>(
    "/api/v1/exports",
    { method: "POST", json: input },
    { idempotencyKey: options?.idempotencyKey }
  );
}

/** 회사 목록 쿼리 파라미터 — 필터 hash가 cursor에 묶이므로 변경 시 새 목록. */
export interface CompanyListFilter {
  q?: string;
  lifecycle_stage?: string;
  owner_principal_id?: string;
  /** API filter selects archived rows only when true; default is active rows. */
  archived?: string;
  /** 서버 정렬 — created_desc(기본) | updated_desc | name_asc. 페이지 전체에 적용된다. */
  sort?: string;
}

export function companyListParams(f: CompanyListFilter): ListParams {
  return {
    q: f.q || undefined,
    lifecycle_stage: f.lifecycle_stage || undefined,
    owner_principal_id: f.owner_principal_id || undefined,
    archived: f.archived || undefined,
    sort: f.sort && f.sort !== "created_desc" ? f.sort : undefined,
  };
}
