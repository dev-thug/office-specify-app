"use client";

import { useTranslations } from "next-intl";

import { Button } from "@/components/ui/button";

/**
 * 수신 거부 확인 — GET은 상태를 바꾸지 않으며, 실제 차단은 POST에서만 일어난다.
 * API 배선은 컴플라이언스 태스크에서 한다 — 성공을 흉내 내지 않고
 * 준비 중임을 명시한다.
 */
export function UnsubscribeConfirm() {
  const t = useTranslations("public.unsubscribe");

  return (
    <div className="flex flex-col gap-5">
      <div>
        <h1 className="text-2xl font-semibold">{t("title")}</h1>
        <p className="mt-1 text-sm text-muted-foreground">{t("description")}</p>
      </div>
      <div className="rounded-2xl border border-border bg-card px-4 py-3 text-sm">
        <span className="text-muted-foreground">{t("scope")}: </span>
        {/* 실제 범위는 /public/unsubscribe/:token 확인 응답에서 채운다 */}
        <span className="font-medium">—</span>
      </div>
      <Button type="button" variant="destructive" className="w-full" disabled>
        {t("confirm")}
      </Button>
      <p className="text-xs text-muted-foreground">{t("notAvailable")}</p>
    </div>
  );
}
