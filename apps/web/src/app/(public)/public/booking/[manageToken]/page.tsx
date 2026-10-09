import type { Metadata } from "next";
import { useTranslations } from "next-intl";

import { StatusBadge } from "@/components/patterns/status-badge";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";

export const metadata: Metadata = {
  robots: { index: false, follow: false },
};

/**
 * 예약 관리 링크 — 토큰만으로 GET 조회한다(로그인 불필요).
 * confirming 은 "확정 중"으로 표시하고 confirmed 와 구분한다. 새로고침해도 동일 상태.
 */
export default function PublicBookingManagePage() {
  const t = useTranslations("public.bookingManage");
  return (
    <div className="flex flex-col gap-5">
      <div>
        <h1 className="text-2xl font-semibold">{t("title")}</h1>
        <p className="mt-1 text-sm text-muted-foreground">{t("description")}</p>
      </div>
      <div className="flex items-center gap-2 rounded-2xl border border-border bg-card px-4 py-3 text-sm">
        <span className="text-muted-foreground">{t("status")}</span>
        {/* 예시 상태 — 실제 값은 /public/bookings/:manageToken 조회로 채운다 */}
        <StatusBadge status="confirming" />
      </div>
      <p className="text-xs text-muted-foreground">{t("confirmingNote")}</p>
      <dl className="grid gap-2 rounded-2xl border border-border bg-card px-4 py-3">
        <Skeleton className="h-4 w-2/3" />
        <Skeleton className="h-4 w-1/2" />
        <Skeleton className="h-4 w-3/5" />
      </dl>
      <div className="flex gap-2">
        <Button type="button" variant="outline" size="sm" disabled>
          {t("change")}
        </Button>
        <Button type="button" variant="destructive" size="sm" disabled>
          {t("cancel")}
        </Button>
      </div>
    </div>
  );
}
