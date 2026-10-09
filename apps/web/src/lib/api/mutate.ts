/**
 * 비-GET 뮤테이션 공통 헬퍼 (spec §4.4, §4.10).
 * - 저장해 둔 리소스 ETag/버전을 If-Match 로 보낸다 (낙관적 잠금).
 * - 논리 작업당 하나의 Idempotency-Key 를 보낸다 — 호출자가 넘기지 않으면
 *   이 호출용 키를 새로 만들므로, 재시도 안전이 필요한 폼은 useRef 로 키를
 *   보유해 넘겨야 한다 (DESIGN.md "API usage").
 */
import {
  apiFetch,
  idempotencyKey,
  ifMatch,
  type ApiResult,
} from "./client";

export interface MutateInit {
  method: "POST" | "PUT" | "PATCH" | "DELETE";
  json?: unknown;
  /** json 대신 원시 body 가 필요할 때. */
  body?: BodyInit | null;
  signal?: AbortSignal;
}

export interface MutateOptions {
  /**
   * 저장된 ETag 값 — GET 응답 헤더(readEtag)나 리소스의 version 필드.
   * 지정하면 If-Match: "<v>" 를 보내 412 VERSION_MISMATCH 를 감지한다.
   */
  etag?: string | number;
  /** 논리 작업당 하나 — 호출자가 useRef 등으로 보유한다. 생략 시 호출마다 새 키. */
  idempotencyKey?: string;
  /** 추가 헤더 — If-Match/Idempotency-Key 와 같은 이름이면 명시 값이 우선한다. */
  headers?: Record<string, string>;
}

export function mutate<T = unknown>(
  path: string,
  init: MutateInit,
  options?: MutateOptions
): Promise<ApiResult<T>> {
  const { method, json, body, signal } = init;
  const headers: Record<string, string> = {};
  if (options?.etag !== undefined) Object.assign(headers, ifMatch(options.etag));
  headers["Idempotency-Key"] = options?.idempotencyKey ?? idempotencyKey();
  Object.assign(headers, options?.headers);
  return apiFetch<T>(path, { method, json, body, signal, headers });
}

/** GET 응답(ApiResult 또는 Response)에서 ETag 헤더를 꺼내 mutate.etag 로 넘긴다. */
export function readEtag(
  source: Response | ApiResult<unknown>
): string | undefined {
  const response = source instanceof Response ? source : source.response;
  return response.headers.get("etag") ?? undefined;
}
