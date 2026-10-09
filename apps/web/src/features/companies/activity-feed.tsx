"use client";

/**
 * 활동 타임라인 — GET /companies/{id}/activities 의 허용 활동만 그린다 (§5.6:
 * 권한 없는 메일 활동은 존재 자체가 노출되지 않는다). group 필터로
 * Activity/Emails/Meetings/Notes/Research 탭이 같은 원천을 나눠 쓴다.
 */
import Link from "next/link";
import { useTranslations, useLocale } from "next-intl";
import {
  CalendarDays,
  Circle,
  ClipboardCheck,
  FlaskConical,
  ListChecks,
  Mail,
  Megaphone,
  ShieldOff,
  StickyNote,
  type LucideIcon,
} from "lucide-react";

import { EmptyState } from "@/components/patterns/empty-state";
import { ErrorState } from "@/components/patterns/error-state";
import { StatusBadge } from "@/components/patterns/status-badge";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import {
  activityGroup,
  activityLabel,
  activityStatus,
  formatDateTime,
  type ActivityGroup,
} from "@/features/companies/utils";
import type { CompanyActivity } from "@/lib/api/companies";
import { ApiError } from "@/lib/api/problem";
import { useCursorList } from "@/lib/api/use-cursor-list";

const GROUP_ICON: Record<ActivityGroup, LucideIcon> = {
  mail: Mail,
  booking: CalendarDays,
  note: StickyNote,
  task: ListChecks,
  research: FlaskConical,
  suppression: ShieldOff,
  approval: ClipboardCheck,
  campaign: Megaphone,
  other: Circle,
};

export interface ActivityFeedProps {
  /** /api/v1/companies/{id}/activities 같은 커서 목록 경로. */
  path: string;
  /** 메시지 네임스페이스 — "companies" | "contacts" (activity.* 키 공통 구조). */
  ns: "companies" | "contacts";
  /** 탭별 행 필터 — 생략 시 전체. */
  filter?: (a: CompanyActivity) => boolean;
  emptyTitle: string;
  emptyDescription?: string;
  queryKeySuffix: string;
}

export function ActivityFeed({
  path,
  ns,
  filter,
  emptyTitle,
  emptyDescription,
  queryKeySuffix,
}: ActivityFeedProps) {
  const t = useTranslations(`${ns}.activity` as const);
  const tc = useTranslations("common");
  const locale = useLocale();
  const list = useCursorList<CompanyActivity>({
    queryKey: ["activities", path, queryKeySuffix],
    path,
  });

  const rows = (list.data?.pages ?? [])
    .flatMap((p) => p.data ?? [])
    .filter((a) => (filter ? filter(a) : true));
  const lastPage = list.data?.pages?.[list.data.pages.length - 1];

  if (list.isPending) {
    return (
      <div className="flex flex-col gap-2" aria-busy="true">
        {Array.from({ length: 4 }).map((_, i) => (
          <Skeleton key={i} className="h-14 w-full rounded-lg" />
        ))}
      </div>
    );
  }

  if (list.isError) {
    return (
      <ErrorState
        error={list.error instanceof ApiError ? list.error : null}
        onRetry={() => list.refetch()}
      />
    );
  }

  if (rows.length === 0) {
    return <EmptyState title={emptyTitle} description={emptyDescription} />;
  }

  return (
    <div className="flex flex-col gap-3">
      <p className="text-2xs text-muted-foreground">{t("permittedOnly")}</p>
      <ul className="flex flex-col gap-2">
        {rows.map((a) => {
          const group = activityGroup(a);
          const Icon = GROUP_ICON[group];
          const label = activityLabel(a);
          const status = activityStatus(a);
          return (
            <li
              key={a.id}
              className="flex items-start gap-3 rounded-lg border border-border px-3 py-2.5"
            >
              <span className="mt-0.5 flex size-7 shrink-0 items-center justify-center rounded-full bg-muted text-muted-foreground">
                <Icon className="size-3.5" aria-hidden />
              </span>
              <div className="min-w-0 flex-1">
                <div className="flex flex-wrap items-center gap-2">
                  <span className="text-ui font-medium">
                    {t(`kinds.${group}`)}
                  </span>
                  {status ? <StatusBadge status={status} /> : null}
                  <span className="text-2xs text-muted-foreground">
                    {formatDateTime(a.occurred_at, locale)}
                  </span>
                </div>
                {label ? (
                  <p className="mt-0.5 truncate text-ui text-muted-foreground">
                    {label}
                  </p>
                ) : null}
                {a.actor_display_name ? (
                  <p className="mt-0.5 text-2xs text-muted-foreground">
                    {a.actor_display_name}
                  </p>
                ) : null}
              </div>
              {a.thread_id ? (
                <Button variant="ghost" size="xs" asChild>
                  <Link href={`/app/inbox/${a.thread_id}`}>
                    {t("openThread")}
                  </Link>
                </Button>
              ) : null}
            </li>
          );
        })}
      </ul>
      {lastPage?.page?.has_more ? (
        <div className="flex justify-center">
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={list.isFetchingNextPage}
            onClick={() => list.fetchNextPage()}
          >
            {list.isFetchingNextPage
              ? tc("table.loading")
              : tc("table.loadMore")}
          </Button>
        </div>
      ) : null}
    </div>
  );
}
