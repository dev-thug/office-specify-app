"use client";

/**
 * 설정 > 캘린더 — 연결 상태·host별 가시 범위·busy 포함 여부.
 * 비공개 일정 상세는 절대 표시하지 않고 연결 메타데이터만 보인다 (§7.6).
 */
import { useLocale, useTranslations } from "next-intl";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useMemo, useState } from "react";
import { toast } from "sonner";

import { DataTable, type ColumnDef } from "@/components/patterns/data-table";
import { EmptyState } from "@/components/patterns/empty-state";
import { PageHeader } from "@/components/patterns/page-header";
import { StatusBadge } from "@/components/patterns/status-badge";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { idempotencyKey } from "@/lib/api/client";
import {
  authorizationUrl,
  createCalendarConnectionRequest,
  useCalendarConnections,
  usePrincipals,
  type CalendarConnection,
} from "@/lib/api/settings";
import { ConnectUrlDialog } from "@/features/operations/connect-url-dialog";
import { apiErrorText } from "@/features/settings/api-error";
import { formatDate } from "@/features/settings/format";
import { useIsAdmin } from "@/features/settings/use-me";

export function CalendarsScreen() {
  const t = useTranslations("settings.calendars");
  const tSettings = useTranslations("settings");
  const locale = useLocale();
  const isAdmin = useIsAdmin();
  const queryClient = useQueryClient();
  const [connectUrl, setConnectUrl] = useState<string | null>(null);

  const calendars = useCalendarConnections();
  const rows = calendars.data?.pages.flatMap((p) => p.data) ?? [];
  const page = calendars.data?.pages.at(-1)?.page;

  const principals = usePrincipals();
  const names = useMemo(() => {
    const map = new Map<string, string>();
    for (const p of principals.data?.pages.flatMap((pg) => pg.data) ?? []) {
      map.set(p.id, p.name);
    }
    return map;
  }, [principals.data]);

  const connect = useMutation({
    mutationFn: () =>
      createCalendarConnectionRequest({}, { idempotencyKey: idempotencyKey() }),
    onSuccess: (res) => {
      setConnectUrl(authorizationUrl(res.data) ?? null);
      queryClient.invalidateQueries({ queryKey: ["settings", "calendar-connections"] });
    },
    onError: (err) => toast.error(apiErrorText(tSettings, err)),
  });

  const columns: ColumnDef<CalendarConnection>[] = [
    {
      key: "host",
      header: t("columns.host"),
      cell: (row) => (
        <span className="text-sm">
          {names.get(row.host_principal_id) ?? row.host_principal_id.slice(0, 8)}
        </span>
      ),
    },
    {
      key: "calendar",
      header: t("columns.calendar"),
      cell: (row) => <span className="font-mono text-xs">{row.provider_calendar_id}</span>,
    },
    {
      key: "provider",
      header: t("columns.provider"),
      cell: (row) => <span className="text-xs">{row.provider}</span>,
    },
    {
      key: "status",
      header: t("columns.status"),
      cell: (row) => <StatusBadge status={row.status} />,
    },
    {
      key: "busy",
      header: t("columns.busy"),
      cell: (row) => (
        <Badge variant={row.include_in_busy ? "info-subtle" : "secondary"}>
          {row.include_in_busy ? t("busyYes") : t("busyNo")}
        </Badge>
      ),
    },
    {
      key: "createdAt",
      header: t("columns.createdAt"),
      cell: (row) => (
        <span className="whitespace-nowrap text-xs">{formatDate(row.created_at, locale)}</span>
      ),
    },
  ];

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-6 px-6 py-6">
      <PageHeader
        title={t("title")}
        description={t("description")}
        actions={
          isAdmin ? (
            <Button
              type="button"
              size="sm"
              disabled={connect.isPending}
              onClick={() => connect.mutate()}
            >
              {t("connect")}
            </Button>
          ) : undefined
        }
      />
      <p className="text-2xs text-muted-foreground">{t("privacyNotice")}</p>

      <DataTable
        columns={columns}
        rows={rows}
        rowKey={(row) => row.id}
        loading={calendars.isPending}
        error={calendars.error}
        onRetry={() => calendars.refetch()}
        page={page}
        onLoadMore={() => calendars.fetchNextPage()}
        loadingMore={calendars.isFetchingNextPage}
        emptyState={
          <EmptyState title={t("emptyTitle")} description={t("emptyDescription")} />
        }
        caption={t("title")}
      />

      <ConnectUrlDialog
        url={connectUrl}
        open={connectUrl !== null}
        onOpenChange={(open) => !open && setConnectUrl(null)}
      />
    </div>
  );
}
