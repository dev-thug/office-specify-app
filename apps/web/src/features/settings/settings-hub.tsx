"use client";

import Link from "next/link";
import { useTranslations } from "next-intl";

import { PageHeader } from "@/components/patterns/page-header";
import { flattenNav } from "@/lib/nav";

export function SettingsHub() {
  const t = useTranslations();
  const items = flattenNav().filter(
    (item) => item.rootKey === "settings" && item.depth > 0
  );

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-6 py-6">
      <PageHeader
        title={t("nav.settings")}
        description={t("nav.help.features.settings.summary")}
      />
      <section aria-label={t("nav.settings")}>
        <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
          {items.map((item) => (
            <Link
              key={item.href}
              href={item.href}
              className="group flex min-h-28 items-start gap-3 rounded-2xl border border-border bg-card p-4 outline-none transition-colors hover:bg-accent/50 focus-visible:ring-[3px] focus-visible:ring-ring/50"
            >
              <span className="flex size-9 shrink-0 items-center justify-center rounded-xl bg-muted text-muted-foreground">
                <item.icon className="size-4" aria-hidden />
              </span>
              <span className="min-w-0">
                <span className="block font-medium text-foreground">
                  {t(`nav.${item.key}`)}
                </span>
                <span className="mt-1 block text-sm leading-relaxed text-muted-foreground">
                  {t(`nav.help.features.${item.key}.summary`)}
                </span>
              </span>
            </Link>
          ))}
        </div>
      </section>
    </div>
  );
}
