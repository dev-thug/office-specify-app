/**
 * templates API — 소스 §8.1, B.2(템플릿 rows), B.3.5.
 * openapi.json 생성 전 손 타이핑 — 필드명은 부록 B wire 형식(snake_case) 그대로.
 *
 * 주의: B.2에 `GET /email-templates/{id}` 단건 조회는 없다. 템플릿 본명이
 * 필요한 화면은 목록 캐시/커서 스캔으로 해결한다(`useEmailTemplate`).
 */
import { apiFetch, type ApiResult } from "./client";
import { mutate, type MutateOptions } from "./mutate";

export const EMAIL_TEMPLATES_PATH = "/api/v1/email-templates";

// ── DTO ──────────────────────────────────────────────────────────────

/** email_templates 행 (DDL + 목록 응답 관용 필드). */
export interface EmailTemplate {
  id: string;
  name: string;
  created_by?: string;
  archived_at?: string | null;
  version?: number;
  created_at?: string;
  latest_version_no?: number;
  versions_count?: number;
  allowed_actions?: string[];
  [key: string]: unknown;
}

/** variable_schema.properties 의 각 항목 — JSON Schema object 의 부분집합 (§8.1). */
export interface TemplateVariableProp {
  type?: string;
  format?: string;
  maxLength?: number;
  default?: string | number | boolean | null;
  description?: string;
}

/** template_versions.variable_schema — allowlist 렌더러가 치환하는 완전 변수명 목록. */
export interface TemplateVariableSchema {
  type: "object";
  properties: Record<string, TemplateVariableProp>;
  required?: string[];
  additionalProperties?: boolean;
}

export const EMPTY_VARIABLE_SCHEMA: TemplateVariableSchema = {
  type: "object",
  properties: {},
  required: [],
  additionalProperties: false,
};

/** template_versions 행. 본문은 불변 — 수정 대신 새 버전을 만든다. */
export interface TemplateVersion {
  id: string;
  template_id?: string;
  version_no: number;
  subject: string;
  body_text: string;
  body_html?: string | null;
  variable_schema: TemplateVariableSchema;
  content_hash?: string;
  created_by?: string;
  created_at?: string;
  [key: string]: unknown;
}

export interface CreateTemplateInput {
  name: string;
}

/** POST /email-templates/{id}/versions 요청 (B.3.5). */
export interface CreateTemplateVersionInput {
  subject: string;
  body_text: string;
  body_html?: string | null;
  variable_schema: TemplateVariableSchema;
}

/** B.3.5 응답 — 201 + template_version_id/version_no/content_hash. */
export interface CreateTemplateVersionResult {
  template_version_id?: string;
  id?: string;
  version_no?: number;
  content_hash?: string;
  [key: string]: unknown;
}

/** POST /template-versions/{id}/render 요청 — 샘플 변수로 미리 렌더링. */
export interface RenderInput {
  variables: Record<string, unknown>;
}

export interface RenderWarning {
  code?: string;
  field?: string;
  message?: string;
  detail?: string;
}

/** render 응답 — preview + 경고·누락 변수 (B.2 "sample variables → preview, 경고"). */
export interface RenderResult {
  subject?: string;
  body_text?: string;
  body_html?: string | null;
  warnings?: RenderWarning[];
  missing_variables?: string[];
  [key: string]: unknown;
}

// ── 호출 ─────────────────────────────────────────────────────────────

/** 템플릿 논리 이름 생성 — POST /email-templates (201). 멱등 키 필수. */
export function createEmailTemplate(
  input: CreateTemplateInput,
  options?: Pick<MutateOptions, "idempotencyKey">
): Promise<ApiResult<EmailTemplate>> {
  return mutate<EmailTemplate>(
    EMAIL_TEMPLATES_PATH,
    { method: "POST", json: input },
    { idempotencyKey: options?.idempotencyKey }
  );
}

/** 불변 버전 생성 — POST /email-templates/{id}/versions (201). */
export function createTemplateVersion(
  templateId: string,
  input: CreateTemplateVersionInput,
  options?: Pick<MutateOptions, "idempotencyKey">
): Promise<ApiResult<CreateTemplateVersionResult>> {
  return mutate<CreateTemplateVersionResult>(
    `${EMAIL_TEMPLATES_PATH}/${encodeURIComponent(templateId)}/versions`,
    { method: "POST", json: input },
    { idempotencyKey: options?.idempotencyKey }
  );
}

/**
 * 샘플 데이터 렌더 미리보기 — POST /template-versions/{id}/render.
 * 부작용이 없는 동기 미리보기라 멱등 키를 싣지 않는다.
 */
export function renderTemplateVersion(
  versionId: string,
  input: RenderInput
): Promise<ApiResult<RenderResult>> {
  return apiFetch<RenderResult>(
    `/api/v1/template-versions/${encodeURIComponent(versionId)}/render`,
    { method: "POST", json: input }
  );
}
