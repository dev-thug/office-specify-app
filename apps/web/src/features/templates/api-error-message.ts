"use client";

/**
 * ApiError.code → 영역 messages 의 errors.<CODE> 로 대응하는 문구로 변환한다.
 * B.5 코드는 모두 매핑하고, 모르는 코드는 서버 detail/title → errors.UNKNOWN 순으로 떨어진다.
 */
import { useTranslations } from "next-intl";

import { ApiError } from "@/lib/api/problem";

export function useApiErrorMessage(namespace: string) {
  const t = useTranslations(namespace);
  return (err: unknown): string => {
    if (err instanceof ApiError) {
      const key = `errors.${err.code}`;
      if (t.has(key)) return t(key);
      return err.problem.detail ?? err.problem.title ?? t("errors.UNKNOWN");
    }
    if (err instanceof Error) return err.message;
    return t("errors.UNKNOWN");
  };
}
