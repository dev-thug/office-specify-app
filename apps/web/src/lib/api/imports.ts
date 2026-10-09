/**
 * imports 영역 API — stored_objects 업로드·import_jobs·import_rows·exports.
 * B.2 3569–3574행, B.3.13, DDL(stored_objects 2064행, import_jobs 2094행,
 * import_rows 2109행) 기준 손 타이핑.
 *
 * 흐름: upload-requests(비공개 업로드 URL) → 파일 업로드 → POST /imports
 * (mapping·duplicate_strategy → 202 validate job) → GET /imports/{id}·rows
 * (dry-run 결과·행별 오류) → POST commit(validation_hash·invalid_row_policy).
 *
 * 주의 (UI11/B.3.13): 새 주소는 발송 근거 미확인으로 시작하고, 부분 성공은
 * 건수를 명시한다. B.2에 GET /imports 목록이 없어 목록 경로는 가정이다.
 */
import { useInfiniteQuery } from "@tanstack/react-query";

import { apiFetch, type ApiResult } from "./client";
import { mutate } from "./mutate";
import { useCursorList } from "./use-cursor-list";

// ── DTO ────────────────────────────────────────────────────

/** import_jobs.status CHECK. */
export type ImportStatus =
  | "uploaded"
  | "validating"
  | "ready"
  | "committing"
  | "completed"
  | "failed"
  | "canceled";

/** import_rows.status CHECK. */
export type ImportRowStatus =
  | "valid"
  | "invalid"
  | "duplicate"
  | "imported"
  | "skipped";

/** import_jobs.summary JSONB — B.3.13 예시 키 + 결과 건수. */
export interface ImportSummary {
  total_rows?: number;
  valid_rows?: number;
  duplicate_rows?: number;
  invalid_rows?: number;
  imported_rows?: number;
  skipped_rows?: number;
  error_rows?: number;
  [key: string]: unknown;
}

export interface ImportRowError {
  field?: string;
  code?: string;
  detail?: string;
  message?: string;
}

export interface ImportJob {
  id: string;
  kind?: "import" | "export" | string;
  stored_object_id: string;
  requested_by: string;
  mapping?: { columns?: Record<string, string>; has_header?: boolean; duplicate_strategy?: "review" | "link" };
  status: ImportStatus;
  /** BYTEA → wire 는 sha256 hex 문자열 (B.1.1 Hash). commit 시 그대로 돌려보낸다. */
  validation_hash?: string | null;
  summary: ImportSummary;
  version: number;
  created_at: string;
  download_url?: string | null;
  [key: string]: unknown;
}

export interface ImportAccepted {
  import_id: string;
  status: ImportStatus;
}

export interface ImportRow {
  row_no: number;
  normalized_data: Record<string, unknown>;
  status: ImportRowStatus;
  errors: ImportRowError[];
  company_id?: string | null;
  contact_id?: string | null;
  created_at?: string;
}

/** POST /imports/upload-requests 입력·결과 — "비공개 업로드 정보" (B.2 3569행). */
export interface UploadRequestInput {
  filename: string;
  media_type: string;
  byte_size: number;
}

export interface UploadRequestData {
  storage_key: string;
  upload_url: string;
  upload_token: string;
  max_bytes: number;
  expires_at?: string;
}

export interface StoredObjectData {
  object_id: string;
  storage_key: string;
  media_type: string;
  byte_size: number;
  content_hash: string;
}

export interface CreateImportInput {
  stored_object_id: string;
  mapping: {
    columns: Record<string, string>;
    has_header: boolean;
    duplicate_strategy: "review" | "link";
  };
}

/** POST /imports/{id}/commit — 검증 스냅샷 해시 + 오류 행 정책 (B.3.13). */
export interface CommitImportInput {
  validation_hash: string;
  invalid_row_policy: "skip" | "reject";
}

/** POST /exports — 필터·필드 → 202 export job (B.2 3574행). */
export interface CreateExportInput {
  fields: string[];
  filters?: Record<string, unknown>;
}

export interface ExportJobData {
  import_id?: string;
  job_id?: string;
  export_job_id?: string;
  [key: string]: unknown;
}

// ── 경로 ────────────────────────────────────────────────────

/** B.2에 목록 GET 이 없다 — §7.2 목록 규칙에 따라 GET /imports 를 가정한다. */
export const IMPORTS_PATH = "/api/v1/imports";
export const IMPORT_UPLOAD_REQUESTS_PATH = "/api/v1/imports/upload-requests";
export const EXPORTS_PATH = "/api/v1/exports";

export function importPath(id: string): string {
  return `${IMPORTS_PATH}/${encodeURIComponent(id)}`;
}

export function importRowsPath(id: string): string {
  return `${importPath(id)}/rows`;
}

export function importCommitPath(id: string): string {
  return `${importPath(id)}/commit`;
}

/**
 * 가져오기 리소스 식별자 — B.3.13 의 `import_id` 가 canonical 이고, 일부
 * 응답(목록 등)이 `id` 를 쓸 수 있어 순서대로 해석한다.
 */
export function importJobId(job: {
  import_id?: string;
  id?: string;
}): string {
  return job.import_id ?? job.id ?? "";
}

// ── 쿼리 훅·뮤테이션 ─────────────────────────────────────────

/** 가져오기 목록 (가정 경로 — report 의 "assumptions" 참조). */
export function useImportJobs() {
  return useCursorList<ImportJob>({
    queryKey: ["imports"],
    path: IMPORTS_PATH,
  });
}

export function getImport(id: string): Promise<ApiResult<ImportJob>> {
  return apiFetch<ImportJob>(importPath(id));
}

export function getImportRowsPage(
  importId: string,
  options: { status?: ImportRowStatus; afterRowNo?: number; limit?: number } = {}
): Promise<ApiResult<ImportRow[]>> {
  const search = new URLSearchParams({
    limit: String(options.limit ?? 50),
    after_row_no: String(options.afterRowNo ?? 0),
  });
  if (options.status) search.set("status", options.status);
  return apiFetch<ImportRow[]>(`${importRowsPath(importId)}?${search.toString()}`);
}

/** Row paging uses after_row_no instead of the opaque cursor contract. */
export function useImportRows(
  importId: string,
  params?: { status?: ImportRowStatus | "all"; enabled?: boolean }
) {
  return useInfiniteQuery({
    queryKey: ["import-rows", importId, params?.status ?? "all"],
    enabled: params?.enabled,
    initialPageParam: 0,
    queryFn: async ({ pageParam }): Promise<ApiResult<ImportRow[]>> =>
      getImportRowsPage(importId, {
        status: params?.status && params.status !== "all" ? params.status : undefined,
        afterRowNo: pageParam,
        limit: 50,
      }),
    getNextPageParam: (lastPage) => {
      if (!lastPage.page?.has_more || !lastPage.page.next_cursor) return undefined;
      const next = Number(lastPage.page.next_cursor);
      return Number.isInteger(next) ? next : undefined;
    },
  });
}

/** 1단계 — 비공개 업로드 URL 발급. */
export function createUploadRequest(
  input: UploadRequestInput,
  options?: { idempotencyKey?: string }
): Promise<ApiResult<UploadRequestData>> {
  return mutate<UploadRequestData>(
    IMPORT_UPLOAD_REQUESTS_PATH,
    { method: "POST", json: input },
    { idempotencyKey: options?.idempotencyKey }
  );
}

/** PUT the raw file bytes through the app origin using the upload capability. */
export async function uploadObject(
  request: UploadRequestData,
  file: Blob
): Promise<StoredObjectData> {
  if (file.size > request.max_bytes) throw new Error("upload exceeds the allowed size");
  const url = new URL(request.upload_url, window.location.origin);
  if (url.origin !== window.location.origin) throw new Error("upload URL must use the application origin");
  url.searchParams.set("token", request.upload_token);
  const response = await apiFetch<StoredObjectData>(`${url.pathname}${url.search}`, {
    method: "PUT",
    body: file,
  });
  return response.data;
}

/** 3단계 — 검증(dry-run) 작업 생성 → 202. */
export function createImport(
  input: CreateImportInput,
  options?: { idempotencyKey?: string }
): Promise<ApiResult<ImportAccepted>> {
  return mutate<ImportAccepted>(
    IMPORTS_PATH,
    { method: "POST", json: input },
    { idempotencyKey: options?.idempotencyKey }
  );
}

/**
 * 커밋 — validation_hash 가 없는 검증 전 상태에서는 호출하지 않는다.
 * import_jobs.version 을 If-Match 로 보내 검증 이후 상태 변경(412)을 감지한다.
 */
export function commitImport(
  id: string,
  input: CommitImportInput,
  options?: { idempotencyKey?: string }
): Promise<ApiResult<ImportAccepted>> {
  return mutate<ImportAccepted>(
    importCommitPath(id),
    { method: "POST", json: input },
    { idempotencyKey: options?.idempotencyKey }
  );
}

/** 비동기 export 작업 — 완료는 /app/operations/jobs 에서 추적한다. */
export function createExport(
  input: CreateExportInput,
  options?: { idempotencyKey?: string }
): Promise<ApiResult<ExportJobData>> {
  return mutate<ExportJobData>(
    EXPORTS_PATH,
    { method: "POST", json: input },
    { idempotencyKey: options?.idempotencyKey }
  );
}
