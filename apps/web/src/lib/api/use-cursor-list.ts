"use client";

/**
 * 커서 목록 공통 훅 (spec §4.3) — {data, page:{next_cursor, has_more}} envelope 를
 * TanStack Query useInfiniteQuery 로 페이지 단위로 누적한다.
 * page.data 가 행 배열이므로 pages.flatMap((p) => p.data) 로 평탄화해 쓴다.
 */
import { useInfiniteQuery, type QueryKey } from "@tanstack/react-query";

import { apiFetch, type ApiResult } from "./client";

export const DEFAULT_LIST_LIMIT = 50;

export type ListParams = Record<
  string,
  string | number | boolean | null | undefined
>;

export interface UseCursorListOptions {
  /** 영역 접두어를 붙인 쿼리 키 — 예: ["companies", { stage }] */
  queryKey: QueryKey;
  /** /api/v1/... 경로. 이미 ? 가 있어도 안전하게 이어붙인다. */
  path: string;
  /** 고정 필터·정렬 파라미터 — cursor/limit 은 훅이 관리한다. */
  params?: ListParams;
  /** 페이지 크기 — 기본 50 (spec §4.3 기본값). */
  limit?: number;
  enabled?: boolean;
  staleTime?: number;
}

export function useCursorList<TItem>({
  queryKey,
  path,
  params,
  limit = DEFAULT_LIST_LIMIT,
  enabled,
  staleTime,
}: UseCursorListOptions) {
  return useInfiniteQuery({
    queryKey,
    enabled,
    staleTime,
    initialPageParam: null as string | null,
    queryFn: async ({ pageParam }): Promise<ApiResult<TItem[]>> => {
      const qs = new URLSearchParams();
      for (const [key, value] of Object.entries(params ?? {})) {
        if (value === undefined || value === null) continue;
        qs.set(key, String(value));
      }
      qs.set("limit", String(limit));
      if (pageParam) qs.set("cursor", pageParam);
      const sep = path.includes("?") ? "&" : "?";
      return apiFetch<TItem[]>(`${path}${sep}${qs.toString()}`);
    },
    getNextPageParam: (last) =>
      last.page?.has_more ? (last.page.next_cursor ?? undefined) : undefined,
  });
}
