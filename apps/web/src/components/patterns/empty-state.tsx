import { Inbox, type LucideIcon } from "lucide-react";
import type { ReactNode } from "react";

import { cn } from "@/lib/utils";

interface EmptyStateProps {
  icon?: LucideIcon;
  title: string;
  description?: string;
  action?: ReactNode;
  /** 좁은 패널용 — 패딩과 아이콘을 줄인다. */
  compact?: boolean;
  className?: string;
}

/** 빈 목록·빈 영역 공통 표시 — 아이콘 + 제목 + 설명 + 선택 액션. */
export function EmptyState({
  icon: Icon = Inbox,
  title,
  description,
  action,
  compact = false,
  className,
}: EmptyStateProps) {
  return (
    <div
      data-slot="empty-state"
      className={cn(
        "flex h-full flex-col items-center justify-center gap-2 rounded-2xl border border-dashed border-border text-center",
        compact ? "px-4 py-6" : "px-6 py-12",
        className
      )}
    >
      <div
        className={cn(
          "flex items-center justify-center rounded-xl bg-muted text-muted-foreground",
          compact ? "size-8" : "size-10"
        )}
      >
        <Icon className={compact ? "size-4" : "size-5"} aria-hidden />
      </div>
      <p className="text-sm font-medium text-foreground">{title}</p>
      {description ? (
        <p className="max-w-md text-sm text-muted-foreground">{description}</p>
      ) : null}
      {action ? <div className="mt-2">{action}</div> : null}
    </div>
  );
}
