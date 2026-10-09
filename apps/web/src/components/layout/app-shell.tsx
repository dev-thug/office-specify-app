"use client";

import { useCallback, useState, type ReactNode } from "react";

import { AppSidebar } from "@/components/layout/app-sidebar";
import {
  CommandPalette,
  useCommandPaletteShortcut,
} from "@/components/layout/command-palette";
import { TopBar } from "@/components/layout/top-bar";

/**
 * ChatGPT/Codex 셸: app-shell 면 위에 테두리 없는 사이드바,
 * 메인은 8px 떠 있는 인셋 패널(my-2 mr-2 rounded-2xl border bg-app-panel shadow-xs).
 */
export function AppShell({ children }: { children: ReactNode }) {
  const [paletteOpen, setPaletteOpen] = useState(false);
  const openPalette = useCallback(() => setPaletteOpen(true), []);
  useCommandPaletteShortcut(openPalette);

  return (
    <div className="h-dvh-safe bg-app-shell text-foreground">
      <div className="flex h-full">
        <AppSidebar onOpenCommandPalette={openPalette} />
        <main className="min-w-0 flex-1 py-2 pr-2">
          <div className="flex h-full flex-col overflow-hidden rounded-2xl border border-border bg-app-panel shadow-xs">
            <TopBar onOpenCommandPalette={openPalette} />
            <div className="panel-scroll-subtle min-h-0 flex-1 overflow-y-auto">
              {children}
            </div>
          </div>
        </main>
      </div>
      <CommandPalette open={paletteOpen} onOpenChange={setPaletteOpen} />
    </div>
  );
}
