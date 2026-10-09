/**
 * application/problem+json → typed error (spec §4.3, B.1.2).
 *
 * Server shape:
 *   { type: "urn:outreach:problem:<kebab-code>", title, status, code, detail,
 *     instance, request_id, retryable, violations: [{field?, code?, detail?}] }
 */

export interface ApiViolation {
  field?: string;
  code?: string;
  detail?: string;
}

export interface ApiProblem {
  type: string;
  title: string;
  status: number;
  code: string;
  detail?: string;
  instance?: string;
  requestId?: string;
  retryable?: boolean;
  violations: ApiViolation[];
}

export class ApiError extends Error {
  readonly problem: ApiProblem;

  constructor(problem: ApiProblem) {
    super(problem.detail ?? problem.title);
    this.name = "ApiError";
    this.problem = problem;
  }

  get status(): number {
    return this.problem.status;
  }

  get code(): string {
    return this.problem.code;
  }

  get requestId(): string | undefined {
    return this.problem.requestId;
  }
}

export function isProblemContentType(contentType: string | null): boolean {
  return contentType !== null && contentType.includes("application/problem+json");
}

const FALLBACK_TITLE = "Request failed";

function asString(value: unknown): string | undefined {
  return typeof value === "string" && value.length > 0 ? value : undefined;
}

function parseViolations(value: unknown): ApiViolation[] {
  if (!Array.isArray(value)) return [];
  return value
    .filter((v): v is Record<string, unknown> => typeof v === "object" && v !== null)
    .map((v) => ({
      field: asString(v.field),
      code: asString(v.code),
      detail: asString(v.detail ?? v.message),
    }));
}

/**
 * 응답 본문을 ApiProblem 으로 정규화한다. problem+json이 아니거나 형태가 다르면
 * status/헤더에서 얻는 최소 정보만 채운다.
 */
export function parseProblem(
  status: number,
  body: unknown,
  options?: { requestId?: string }
): ApiProblem {
  const fallback: ApiProblem = {
    type: "about:blank",
    title: FALLBACK_TITLE,
    status,
    code: status === 429 ? "RATE_LIMITED" : status >= 500 ? "INTERNAL" : "UNKNOWN",
    requestId: options?.requestId,
    violations: [],
  };

  if (typeof body !== "object" || body === null) return fallback;

  const b = body as Record<string, unknown>;
  const resolvedStatus =
    typeof b.status === "number" && Number.isInteger(b.status) ? b.status : status;

  return {
    type: asString(b.type) ?? fallback.type,
    title: asString(b.title) ?? fallback.title,
    status: resolvedStatus,
    code: asString(b.code) ?? fallback.code,
    detail: asString(b.detail),
    instance: asString(b.instance),
    requestId: asString(b.request_id) ?? options?.requestId,
    retryable: typeof b.retryable === "boolean" ? b.retryable : undefined,
    violations: parseViolations(b.violations),
  };
}
