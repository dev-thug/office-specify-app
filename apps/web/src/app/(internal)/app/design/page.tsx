import { useTranslations } from "next-intl";
import { notFound } from "next/navigation";

import { PageHeader } from "@/components/patterns/page-header";
import { cn } from "@/lib/utils";

import { DesignGallery } from "./design-gallery";

/** 개발 전용 프리미티브·패턴 카탈로그 — 두 테마를 나란히 띄운다. */
export default function DesignPage() {
  if (process.env.NODE_ENV !== "development") notFound();
  const t = useTranslations("screens.design");
  const td = useTranslations("design");

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-6 px-6 py-6">
      <PageHeader title={t("title")} description={t("description")} />
      <div className="grid gap-4 xl:grid-cols-2">
        {([undefined, "dark"] as const).map((scope) => (
          <div
            key={scope ?? "light"}
            className={cn(
              scope,
              "rounded-3xl border border-border bg-background p-5"
            )}
          >
            <p className="mb-4 text-xs font-semibold uppercase tracking-wider text-muted-foreground">
              {td(scope === "dark" ? "dark" : "light")}
            </p>
            <DesignGallery />
          </div>
        ))}
      </div>
    </div>
  );
}
