"use client";

import { useTranslations } from "next-intl";
import { usePathname } from "next/navigation";

import { ThemeToggle } from "@/components/layout/theme-toggle";
import { UserMenu } from "@/components/layout/user-menu";
import { flattenNav } from "@/lib/nav";

/** 패널 상단 바 — 현재 영역 이름 + 테마 + 사용자 메뉴. */
export function TopBar() {
  const t = useTranslations();
  const pathname = usePathname();
  const current = flattenNav()
    .filter((item) => pathname === item.href || pathname.startsWith(`${item.href}/`))
    .sort((a, b) => b.href.length - a.href.length)[0];

  return (
    <header className="flex h-13 shrink-0 items-center justify-between gap-3 border-b border-app-border-subtle px-4">
      <div className="min-w-0 text-ui font-medium text-muted-foreground">
        {current ? t(`nav.${current.key}`) : ""}
      </div>
      <div className="flex shrink-0 items-center gap-1">
        <ThemeToggle />
        <UserMenu />
      </div>
    </header>
  );
}
