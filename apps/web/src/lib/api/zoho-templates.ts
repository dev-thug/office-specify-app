import { apiFetch, type ApiResult } from "./client";
import { mutate, type MutateOptions } from "./mutate";

/** Zoho Email API is a separately configured prepaid product, not Campaigns OAuth. */
export const ZOHO_EMAIL_TEMPLATES_PATH = "/api/v1/zoho-email-api/templates";

export interface ZohoEmailTemplateSummary {
  id: string;
  name: string;
}

export type ZohoEmailTemplateContentType = "html" | "text";

export interface ZohoEmailTemplate extends ZohoEmailTemplateSummary {
  content: string;
  content_type: ZohoEmailTemplateContentType;
  content_hash: string;
}

export interface ZohoEmailTemplateWriteInput {
  name: string;
  content: string;
  content_type: ZohoEmailTemplateContentType;
}

export interface ZohoEmailTemplateMutationResult {
  operation_id: string;
  template_id: string | null;
  deleted: boolean;
}

/** The route uses a signed cursor and accepts at most 100 summaries per page. */
export function listZohoEmailTemplates(options: {
  cursor?: string | null;
  limit?: number;
} = {}): Promise<ApiResult<ZohoEmailTemplateSummary[]>> {
  const query = new URLSearchParams();
  query.set("limit", String(Math.min(100, Math.max(1, options.limit ?? 100))));
  if (options.cursor) query.set("cursor", options.cursor);
  return apiFetch<ZohoEmailTemplateSummary[]>(
    `${ZOHO_EMAIL_TEMPLATES_PATH}?${query.toString()}`
  );
}

export function getZohoEmailTemplate(
  id: string
): Promise<ApiResult<ZohoEmailTemplate>> {
  return apiFetch<ZohoEmailTemplate>(
    `${ZOHO_EMAIL_TEMPLATES_PATH}/${encodeURIComponent(id)}`
  );
}

export function createZohoEmailTemplate(
  input: ZohoEmailTemplateWriteInput,
  options?: Pick<MutateOptions, "idempotencyKey">
): Promise<ApiResult<ZohoEmailTemplateMutationResult>> {
  return mutate<ZohoEmailTemplateMutationResult>(
    ZOHO_EMAIL_TEMPLATES_PATH,
    { method: "POST", json: input },
    { idempotencyKey: options?.idempotencyKey }
  );
}

export function updateZohoEmailTemplate(
  id: string,
  input: ZohoEmailTemplateWriteInput,
  options: Pick<MutateOptions, "idempotencyKey" | "etag">
): Promise<ApiResult<ZohoEmailTemplateMutationResult>> {
  return mutate<ZohoEmailTemplateMutationResult>(
    `${ZOHO_EMAIL_TEMPLATES_PATH}/${encodeURIComponent(id)}`,
    { method: "PUT", json: input },
    options
  );
}

export function deleteZohoEmailTemplate(
  id: string,
  options: Pick<MutateOptions, "idempotencyKey" | "etag">
): Promise<ApiResult<ZohoEmailTemplateMutationResult>> {
  return mutate<ZohoEmailTemplateMutationResult>(
    `${ZOHO_EMAIL_TEMPLATES_PATH}/${encodeURIComponent(id)}`,
    { method: "DELETE" },
    options
  );
}

/** Zoho uses `$[Field Name]$`; unresolved fields must block Office import. */
export function zohoMergeFields(source: string): string[] {
  return [...new Set(Array.from(source.matchAll(/\$\[([^\]]+)\]\$/g), (match) => match[1]!.trim()))];
}

export type ZohoMergeFieldMapping = string | "remove";

export function mapZohoMergeFields(
  source: string,
  mappings: Record<string, ZohoMergeFieldMapping | undefined>
): { content: string; unresolved: string[] } {
  const unresolved = zohoMergeFields(source).filter((name) => !mappings[name]);
  const content = source.replace(/\$\[([^\]]+)\]\$/g, (token, rawName: string) => {
    const mapping = mappings[rawName.trim()];
    if (!mapping) return token;
    return mapping === "remove" ? "" : `{{${mapping}}}`;
  });
  return { content, unresolved };
}
