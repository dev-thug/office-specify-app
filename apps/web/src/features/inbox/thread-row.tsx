"use client";

/** 대화 목록 행 — 제목·스니펫·상대방·상태·담당자·안읽음 표시. */
import { useFormatter, useTranslations } from "next-intl";
import Link from "next/link";

import { StatusBadge } from "@/components/patterns/status-badge";
import { Badge } from "@/components/ui/badge";
import type { EmailThreadSummary } from "@/lib/api/inbox";
import { cn } from "@/lib/utils";

interface ThreadRowProps {
  thread: EmailThreadSummary;
  unread: boolean;
}

export function ThreadRow({ thread, unread }: ThreadRowProps) {
  const t = useTranslations("inbox");
  const format = useFormatter();

  const counterpart =
    thread.contact_name ??
    thread.company_name ??
    null;
  const when = thread.last_message_at ?? thread.updated_at;

  return (
    <Link
      href={`/app/inbox/${thread.id}`}
      aria-label={t("list.open")}
      className={cn(
        "flex flex-col gap-1.5 rounded-2xl border border-border bg-card px-4 py-3",
        "transition-colors hover:bg-muted/50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
      )}
    >
      <div className="flex items-center gap-2">
        {unread ? (
          <Badge variant="info-subtle" className="shrink-0">
            {t("list.unread")}
          </Badge>
        ) : null}
        <span
          className={cn(
            "min-w-0 flex-1 truncate text-sm",
            unread ? "font-semibold" : "font-medium"
          )}
        >
          {thread.subject ?? t("list.noSubject")}
        </span>
        <StatusBadge status={thread.status} className="shrink-0" />
      </div>

      <div className="flex items-center gap-2 text-xs text-muted-foreground">
        {counterpart ? (
          <span className="min-w-0 truncate font-medium text-foreground">
            {counterpart}
          </span>
        ) : null}
        {thread.snippet ? (
          <span className="min-w-0 flex-1 truncate">{thread.snippet}</span>
        ) : (
          <span className="flex-1" />
        )}
        <span className="shrink-0">
          {thread.assigned_principal_name ?? t("list.unassigned")}
        </span>
        <time
          dateTime={when}
          className="shrink-0 tabular-nums"
        >
          {format.dateTime(new Date(when), {
            dateStyle: "medium",
            timeStyle: "short",
          })}
        </time>
      </div>
    </Link>
  );
}
