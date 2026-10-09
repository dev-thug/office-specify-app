"use client";

/**
 * B.2에 GET /email-templates/{id} 단건 조회가 없다. 목록 쿼리 캐시를 먼저 보고,
 * 없으면 커서를 따라가며(상한 10페이지 × 200건) 해당 id를 찾는다.
 */
import {
  useQuery,
  useQueryClient,
  type InfiniteData,
} from "@tanstack/react-query";

import { apiFetch, type ApiResult } from "@/lib/api/client";
import { EMAIL_TEMPLATES_PATH, type EmailTemplate } from "@/lib/api/templates";

const MAX_PAGES = 10;
const PAGE_LIMIT = 200;

export function useEmailTemplate(id: string | undefined) {
  const qc = useQueryClient();
  return useQuery({
    queryKey: ["templates", "detail", id],
    enabled: Boolean(id),
    queryFn: async (): Promise<EmailTemplate | null> => {
      // 1) 목록 화면에서 쌓인 커서 캐시를 먼저 스캔한다.
      const lists = qc.getQueriesData<
        InfiniteData<ApiResult<EmailTemplate[]>>
      >({ queryKey: ["templates", "list"] });
      for (const [, data] of lists) {
        for (const page of data?.pages ?? []) {
          const hit = page.data.find((t) => t.id === id);
          if (hit) return hit;
        }
      }
      // 2) 캐시 미스 → 문서화된 목록 커서를 따라가며 찾는다.
      let cursor: string | null = null;
      for (let i = 0; i < MAX_PAGES; i++) {
        const qs = new URLSearchParams({ limit: String(PAGE_LIMIT) });
        if (cursor) qs.set("cursor", cursor);
        const res = await apiFetch<EmailTemplate[]>(
          `${EMAIL_TEMPLATES_PATH}?${qs.toString()}`
        );
        const hit = res.data.find((t) => t.id === id);
        if (hit) return hit;
        if (!res.page?.has_more || !res.page.next_cursor) break;
        cursor = res.page.next_cursor;
      }
      return null;
    },
  });
}
