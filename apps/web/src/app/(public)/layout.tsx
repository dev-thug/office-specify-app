import type { Metadata } from "next";
import { useTranslations } from "next-intl";
import Image from "next/image";
import type { ReactNode } from "react";

export const metadata: Metadata = {
  robots: { index: false, follow: false, nocache: true },
  referrer: "no-referrer",
};

/** 공개 라우트 — 앱 크롬 없는 최소 중앙 레이아웃 + noindex. */
export default function PublicLayout({ children }: { children: ReactNode }) {
  const t = useTranslations("app");
  return (
    <div className="flex min-h-dvh flex-col bg-background">
      <header className="flex items-center justify-center gap-2 border-b border-border px-4 py-4">
        <Image
          src="/logo.png"
          alt={t("name")}
          width={28}
          height={28}
          className="rounded-lg"
        />
        <span className="text-sm font-semibold">{t("name")}</span>
      </header>
      <main className="mx-auto flex w-full max-w-xl flex-1 flex-col px-4 py-8">
        {children}
      </main>
    </div>
  );
}
