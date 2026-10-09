"use client";

/**
 * 브라우저용 API 클라이언트 (spec §4.10).
 * - 같은 출처로만 호출한다 — /api/v1/* 는 next.config.ts rewrite가 API로 전달한다.
 * - 세션 인증 비-GET 요청에는 X-CSRF-Token = outreach_csrf 쿠키 값을 넣는다 (double submit).
 * - Idempotency-Key / If-Match 헬퍼를 제공한다.
 * - application/problem+json 응답은 ApiError 로 변환한다.
 */
import createClient from "openapi-fetch";

import { ApiError, isProblemContentType, parseProblem } from "./problem";
import type { paths } from "./schema";

export const SESSION_COOKIE = "outreach_session";
export const CSRF_COOKIE = "outreach_csrf";
export const CSRF_HEADER = "X-CSRF-Token";
export const IDEMPOTENCY_HEADER = "Idempotency-Key";
export const IF_MATCH_HEADER = "If-Match";

export function readCookie(name: string): string | undefined {
  if (typeof document === "undefined") return undefined;
  const prefix = `${encodeURIComponent(name)}=`;
  for (const part of document.cookie.split(";")) {
    const trimmed = part.trim();
    if (trimmed.startsWith(prefix)) {
      return decodeURIComponent(trimmed.slice(prefix.length));
    }
  }
  return undefined;
}

export function csrfToken(): string | undefined {
  return readCookie(CSRF_COOKIE);
}

/** 새 멱등 키 (B.1.3, ≤128자). */
export function idempotencyKey(): string {
  return crypto.randomUUID();
}

/** 낙관적 잠금 헤더 — ETag "version" 형식 (§4.4). */
export function ifMatch(version: number | string): Record<string, string> {
  const v = typeof version === "number" ? String(version) : version;
  return { [IF_MATCH_HEADER]: `"${v.replace(/^"|"$/g, "")}"` };
}

export function withIdempotency(key?: string): Record<string, string> {
  return { [IDEMPOTENCY_HEADER]: key ?? idempotencyKey() };
}

type FetchInput = RequestInfo | URL;

function isSessionProtectedMethod(method: string | undefined): boolean {
  const m = (method ?? "GET").toUpperCase();
  return m !== "GET" && m !== "HEAD" && m !== "OPTIONS";
}

/** CSRF 헤더를 심고 problem+json 을 ApiError 로 변환하는 공통 fetch 래퍼. */
async function outreachFetch(
  input: FetchInput,
  init?: RequestInit
): Promise<Response> {
  const headers = new Headers(init?.headers);
  if (isSessionProtectedMethod(init?.method) && !headers.has(CSRF_HEADER)) {
    const token = csrfToken();
    if (token) headers.set(CSRF_HEADER, token);
  }
  if (!headers.has("Accept")) headers.set("Accept", "application/json");

  const response = await fetch(input, { ...init, headers });
  if (!response.ok && isProblemContentType(response.headers.get("content-type"))) {
    const body = await response
      .clone()
      .json()
      .catch(() => null);
    throw new ApiError(
      parseProblem(response.status, body, {
        requestId: response.headers.get("x-request-id") ?? undefined,
      })
    );
  }
  return response;
}

/** 성공 envelope `{data, meta, page?}` 를 풀어 data(+meta/page)를 반환하는 JSON 헬퍼. */
export interface Envelope<T> {
  data: T;
  meta?: { request_id?: string } & Record<string, unknown>;
  page?: { next_cursor: string | null; has_more: boolean };
}

export interface ApiResult<T> {
  data: T;
  meta?: Envelope<T>["meta"];
  page?: Envelope<T>["page"];
  response: Response;
}

export async function apiFetch<T>(
  path: string,
  init?: RequestInit & { json?: unknown }
): Promise<ApiResult<T>> {
  const { json, ...rest } = init ?? {};
  const headers = new Headers(rest.headers);
  let body = rest.body;
  if (json !== undefined) {
    headers.set("Content-Type", "application/json");
    body = JSON.stringify(json);
  }
  const response = await outreachFetch(path, { ...rest, headers, body });
  if (!response.ok) {
    throw new ApiError(
      parseProblem(response.status, null, {
        requestId: response.headers.get("x-request-id") ?? undefined,
      })
    );
  }
  if (response.status === 204) {
    return { data: undefined as T, response };
  }
  const bodyJson = (await response.json().catch(() => null)) as unknown;
  if (
    typeof bodyJson === "object" &&
    bodyJson !== null &&
    "data" in (bodyJson as Record<string, unknown>)
  ) {
    const env = bodyJson as Envelope<T>;
    return { data: env.data, meta: env.meta, page: env.page, response };
  }
  return { data: bodyJson as T, response };
}

/**
 * 생성 스키마용 openapi-fetch 클라이언트. contracts/openapi/openapi.json 이
 * 생성되면 `pnpm gen:api` 로 schema.d.ts 를 갱신해 경로·본문 타입이 바인딩된다.
 */
export const apiClient = createClient<paths>({
  baseUrl: "",
  fetch: outreachFetch,
});
