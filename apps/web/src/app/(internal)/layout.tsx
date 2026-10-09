import type { ReactNode } from "react";

import { AppShell } from "@/components/layout/app-shell";

/** 내부 라우트 그룹 — 인증 게이트는 middleware.ts, 여기서는 셸만 그린다. */
export default function InternalLayout({ children }: { children: ReactNode }) {
  return <AppShell>{children}</AppShell>;
}
