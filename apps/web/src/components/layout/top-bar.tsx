"use client";

import { MenuIcon } from "lucide-react";
import { useTranslations } from "next-intl";
import Link from "next/link";
import { usePathname } from "next/navigation";
import { useState } from "react";

import { Logo } from "@/components/layout/logo";
import { SidebarNavContent } from "@/components/layout/app-sidebar";
import { ThemeToggle } from "@/components/layout/theme-toggle";
import { UserMenu } from "@/components/layout/user-menu";
import { Button } from "@/components/ui/button";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import { flattenNav } from "@/lib/nav";

interface TopBarProps {
  onOpenCommandPalette?: () => void;
}

/** 패널 상단 바 — 현재 영역 이름 + 테마 + 사용자 메뉴. md 미만에서는 메뉴 버튼으로 탐색 시트를 연다. */
export function TopBar({ onOpenCommandPalette }: TopBarProps) {
  const t = useTranslations();
  const pathname = usePathname();
  const [navOpen, setNavOpen] = useState(false);
  const current = flattenNav()
    .filter((item) => pathname === item.href || pathname.startsWith(`${item.href}/`))
    .sort((a, b) => b.href.length - a.href.length)[0];

  return (
    <header className="flex h-13 shrink-0 items-center justify-between gap-3 border-b border-app-border-subtle px-4">
      <div className="flex min-w-0 items-center gap-1">
        {/* 모바일 탐색 — md 미만 전용 */}
        <Sheet open={navOpen} onOpenChange={setNavOpen}>
          <Button
            type="button"
            variant="ghost"
            size="icon"
            className="md:hidden"
            aria-label={t("common.mainNav")}
            onClick={() => setNavOpen(true)}
          >
            <MenuIcon className="size-4" />
          </Button>
          <SheetContent side="left" className="w-[260px] gap-0 bg-sidebar p-0">
            <SheetHeader className="h-13 flex-row items-center px-4 py-0">
              <SheetTitle className="sr-only">{t("common.mainNav")}</SheetTitle>
              <SheetDescription className="sr-only">
                {t("common.sidebarLabel")}
              </SheetDescription>
              <Link
                href="/app/dashboard"
                aria-label="Outreach OS"
                onClick={() => setNavOpen(false)}
              >
                <Logo />
              </Link>
            </SheetHeader>
            <SidebarNavContent
              onOpenCommandPalette={() => {
                setNavOpen(false);
                onOpenCommandPalette?.();
              }}
              onNavigate={() => setNavOpen(false)}
            />
          </SheetContent>
        </Sheet>
        <div className="min-w-0 text-ui font-medium text-muted-foreground">
          {current ? t(`nav.${current.key}`) : ""}
        </div>
      </div>
      <div className="flex shrink-0 items-center gap-1">
        <ThemeToggle />
        <UserMenu />
      </div>
    </header>
  );
}
