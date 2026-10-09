import type { Metadata } from "next";
import { useTranslations } from "next-intl";
import { CalendarClockIcon, GlobeIcon, UserIcon } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import { Textarea } from "@/components/ui/textarea";

export const metadata: Metadata = {
  robots: { index: false, follow: false },
};

/**
 * 공개 예약 링크 — GET은 미리보기만 한다(상태 변경 없음).
 * 슬롯·호스트 정보는 후속 태스크에서 /public/booking-links/:token 으로 로드한다.
 */
export default function PublicBookPage() {
  const t = useTranslations("public.book");
  return (
    <div className="flex flex-col gap-5">
      <div>
        <h1 className="text-2xl font-semibold">{t("title")}</h1>
        <p className="mt-1 text-sm text-muted-foreground">{t("description")}</p>
      </div>

      <dl className="grid gap-2 rounded-2xl border border-border bg-card px-4 py-3 text-sm">
        <div className="flex items-center gap-2">
          <UserIcon className="size-4 text-muted-foreground" aria-hidden />
          <dt className="text-muted-foreground">{t("host")}</dt>
          <dd>
            <Skeleton className="inline-block h-4 w-28 align-middle" />
          </dd>
        </div>
        <div className="flex items-center gap-2">
          <GlobeIcon className="size-4 text-muted-foreground" aria-hidden />
          <dt className="text-muted-foreground">{t("timezone")}</dt>
          <dd>
            <Skeleton className="inline-block h-4 w-24 align-middle" />
          </dd>
        </div>
      </dl>

      <section aria-label={t("selectSlot")} className="flex flex-col gap-2">
        <h2 className="flex items-center gap-1.5 text-sm font-medium">
          <CalendarClockIcon className="size-4" aria-hidden />
          {t("selectSlot")}
        </h2>
        <div className="grid grid-cols-2 gap-2 sm:grid-cols-3">
          {Array.from({ length: 6 }).map((_, i) => (
            <Skeleton key={i} className="h-10 w-full" />
          ))}
        </div>
      </section>

      <form className="flex flex-col gap-4">
        <div className="grid gap-1.5">
          <Label htmlFor="book-name">{t("name")}</Label>
          <Input id="book-name" disabled />
        </div>
        <div className="grid gap-1.5">
          <Label htmlFor="book-email">{t("email")}</Label>
          <Input id="book-email" type="email" disabled />
        </div>
        <div className="grid gap-1.5">
          <Label htmlFor="book-memo">{t("memo")}</Label>
          <Textarea id="book-memo" disabled />
        </div>
        <Button type="button" disabled className="w-full">
          {t("submit")}
        </Button>
        <p className="text-xs text-muted-foreground">{t("footnote")}</p>
      </form>
    </div>
  );
}
