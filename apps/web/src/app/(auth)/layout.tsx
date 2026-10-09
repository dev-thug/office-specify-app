import type { Metadata } from "next";
import { useTranslations } from "next-intl";
import Image from "next/image";
import type { ReactNode } from "react";

export const metadata: Metadata = {
  robots: { index: false, follow: false },
};

/** 인증 화면 — 앱 크롬 없이 로고 + 카드. */
export default function AuthLayout({ children }: { children: ReactNode }) {
  const t = useTranslations("app");
  return (
    <div className="flex min-h-dvh flex-col items-center justify-center bg-background px-4 py-10">
      <div className="mb-6 flex flex-col items-center gap-2">
        <Image
          src="/logo.png"
          alt={t("name")}
          width={56}
          height={56}
          className="rounded-2xl shadow-sm"
          priority
        />
        <p className="text-sm font-medium text-muted-foreground">{t("name")}</p>
      </div>
      <div className="w-full max-w-sm">{children}</div>
    </div>
  );
}
