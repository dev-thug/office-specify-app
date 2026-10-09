"use client";

import { Search } from "lucide-react";
import { useTranslations } from "next-intl";
import Link from "next/link";
import { usePathname } from "next/navigation";

import { Logo } from "@/components/layout/logo";
import { ScrollArea } from "@/components/ui/scroll-area";
import { cn } from "@/lib/utils";
import { NAV_SECTIONS } from "@/lib/nav";

function isActive(pathname: string, href: string): boolean {
  if (href === "/app/dashboard") return pathname === href;
  return pathname === href || pathname.startsWith(`${href}/`);
}

interface SidebarNavContentProps {
  onOpenCommandPalette?: () => void;
  /** 모바일 시트에서 행을 누르면 시트를 닫는다. */
  onNavigate?: () => void;
}

/** 검색 진입점 + 섹션별 탐색 — 데스크톱 aside 와 모바일 Sheet 가 공유한다. */
export function SidebarNavContent({
  onOpenCommandPalette,
  onNavigate,
}: SidebarNavContentProps) {
  const t = useTranslations();
  const pathname = usePathname();

  return (
    <>
      {/* 검색 진입점 — Input 과 같은 pill */}
      <div className="px-2 pb-1">
        <button
          type="button"
          aria-label={t("common.search")}
          onClick={onOpenCommandPalette}
          className="flex h-9 w-full items-center gap-2 rounded-full border border-border/70 bg-background/70 px-3.5 text-muted-foreground transition-colors hover:bg-sidebar-accent hover:text-sidebar-accent-foreground"
        >
          <Search className="size-3.5 shrink-0" />
          <span className="min-w-0 flex-1 truncate text-left text-sm">
            {t("common.commandPalette.placeholder")}
          </span>
          <span className="shrink-0 rounded-md border border-border/60 px-1.5 py-0.5 text-[10px] font-medium leading-none text-muted-foreground">
            ⌘K
          </span>
        </button>
      </div>

      <ScrollArea className="mt-1.5 min-h-0 flex-1">
        <nav className="px-2 pb-3" aria-label={t("common.mainNav")}>
          {NAV_SECTIONS.map((section) => (
            <div key={section.key} className="mt-3 first:mt-0">
              {section.key !== "main" ? (
                <div className="px-2.5 pb-1 pt-2 text-2xs font-medium text-muted-foreground">
                  {t(`nav.sections.${section.key}`)}
                </div>
              ) : null}
              <div className="flex flex-col gap-0.5">
                {section.items.map((item) => (
                  <div key={item.key}>
                    <Link
                      href={item.href}
                      onClick={onNavigate}
                      aria-current={isActive(pathname, item.href) ? "page" : undefined}
                      className={cn(
                        "flex h-9 w-full items-center gap-2.5 rounded-lg px-2.5 text-sm leading-none transition-colors",
                        isActive(pathname, item.href)
                          ? "bg-sidebar-accent font-medium text-sidebar-accent-foreground"
                          : "text-muted-foreground hover:bg-sidebar-accent hover:text-sidebar-accent-foreground"
                      )}
                    >
                      <item.icon className="size-4 shrink-0" />
                      <span className="truncate">{t(`nav.${item.key}`)}</span>
                    </Link>
                    {item.children?.map((child) => (
                      <Link
                        key={child.key}
                        href={child.href}
                        onClick={onNavigate}
                        aria-current={isActive(pathname, child.href) ? "page" : undefined}
                        className={cn(
                          "ml-4 flex h-9 w-[calc(100%-1rem)] items-center gap-2.5 rounded-lg px-2.5 text-sm leading-none transition-colors",
                          isActive(pathname, child.href)
                            ? "bg-sidebar-accent font-medium text-sidebar-accent-foreground"
                            : "text-muted-foreground hover:bg-sidebar-accent hover:text-sidebar-accent-foreground"
                        )}
                      >
                        <child.icon className="size-4 shrink-0" />
                        <span className="truncate">{t(`nav.${child.key}`)}</span>
                      </Link>
                    ))}
                  </div>
                ))}
              </div>
            </div>
          ))}
        </nav>
      </ScrollArea>
    </>
  );
}

interface AppSidebarProps {
  onOpenCommandPalette?: () => void;
}

/**
 * ChatGPT/Codex 배치 — 테두리 없는 260px 사이드바가 app-shell 면 위에 놓인다.
 * 헤더 52px(h-13) + 36px 행(h-9, rounded-lg). md 미만에서는 TopBar 의 Sheet 사용.
 */
export function AppSidebar({ onOpenCommandPalette }: AppSidebarProps) {
  const t = useTranslations();

  return (
    <aside
      aria-label={t("common.sidebarLabel")}
      className="group/sidebar relative hidden h-full w-[260px] shrink-0 flex-col bg-sidebar text-sidebar-foreground md:flex"
    >
      {/* 헤더 52px — 로고 + 워드마크 */}
      <div className="flex h-13 shrink-0 items-center px-4">
        <Link href="/app/dashboard" aria-label="Outreach OS">
          <Logo />
        </Link>
      </div>
      <SidebarNavContent onOpenCommandPalette={onOpenCommandPalette} />
    </aside>
  );
}
