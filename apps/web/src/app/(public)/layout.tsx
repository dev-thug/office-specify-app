import type { Metadata } from "next";
import Image from "next/image";
import type { ReactNode } from "react";

/**
 * 공개 라우트(미팅 예약·예약 관리·수신 거부)는 외부 수신자가 보는 화면이라
 * 내부 제품명(Office) 대신 대외 브랜드 Specify를 쓴다.
 */
const PUBLIC_BRAND = { name: "Specify", url: "https://specify.app" } as const;

export const metadata: Metadata = {
  title: {
    absolute: PUBLIC_BRAND.name,
    template: `%s · ${PUBLIC_BRAND.name}`,
  },
  robots: { index: false, follow: false, nocache: true },
  referrer: "no-referrer",
};

/** 공개 라우트 — 앱 크롬 없는 최소 중앙 레이아웃 + noindex. */
export default function PublicLayout({ children }: { children: ReactNode }) {
  return (
    <div className="flex min-h-dvh flex-col bg-background">
      <header className="flex items-center justify-center px-4 pt-5">
        <a
          href={PUBLIC_BRAND.url}
          target="_blank"
          rel="noopener noreferrer"
          className="flex items-center gap-2 rounded-full px-3 py-1 transition-colors hover:bg-accent"
        >
          <Image
            src="/logo.png"
            alt=""
            width={28}
            height={28}
            className="rounded-lg"
          />
          <span className="text-sm font-semibold">{PUBLIC_BRAND.name}</span>
        </a>
      </header>
      <main className="flex w-full flex-1 flex-col px-4 py-8 lg:py-12">
        {children}
      </main>
    </div>
  );
}
