"use client";

import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "@/lib/api/client";
import { EMAIL_TEMPLATES_PATH, type EmailTemplateDetail } from "@/lib/api/templates";

export function useEmailTemplate(id: string | undefined) {
  return useQuery({
    queryKey: ["templates", "detail", id],
    enabled: Boolean(id),
    queryFn: async (): Promise<EmailTemplateDetail> =>
      (await apiFetch<EmailTemplateDetail>(`${EMAIL_TEMPLATES_PATH}/${encodeURIComponent(id!)}`)).data,
  });
}
