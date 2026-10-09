/**
 * 서버 컴포넌트/라우트 핸들러용 API fetch (spec §4.10).
 * 들어온 요청의 쿠키를 그대로 전달하고 인증 응답은 캐시하지 않는다.
 */
import { cookies } from "next/headers";

import { ApiError, parseProblem } from "./problem";

export const API_ORIGIN =
  process.env.API_ORIGIN ?? "http://127.0.0.1:8080";

export interface ServerFetchOptions extends RequestInit {
  /** 비-GET 요청에 X-CSRF-Token 을 넣을지 (session 인증일 때 필수). */
  csrf?: boolean;
}

export async function serverApiFetch<T>(
  path: string,
  options?: ServerFetchOptions
): Promise<T> {
  const cookieStore = await cookies();
  const cookieHeader = cookieStore.toString();

  const headers = new Headers(options?.headers);
  headers.set("Accept", "application/json");
  if (cookieHeader) headers.set("Cookie", cookieHeader);

  const method = (options?.method ?? "GET").toUpperCase();
  if (options?.csrf && method !== "GET" && method !== "HEAD") {
    const csrf = cookieStore.get("outreach_csrf")?.value;
    if (csrf) headers.set("X-CSRF-Token", csrf);
  }

  const init: RequestInit = { ...options };
  delete (init as Partial<ServerFetchOptions>).csrf;
  const response = await fetch(`${API_ORIGIN}${path}`, {
    ...init,
    headers,
    cache: "no-store",
  });

  const body = await response.json().catch(() => null);

  if (!response.ok) {
    throw new ApiError(
      parseProblem(response.status, body, {
        requestId: response.headers.get("x-request-id") ?? undefined,
      })
    );
  }

  if (
    typeof body === "object" &&
    body !== null &&
    "data" in (body as Record<string, unknown>)
  ) {
    return (body as { data: T }).data;
  }
  return body as T;
}
