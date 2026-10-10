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
  createLocalCalendarConnection,
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
  const tc = useTranslations("common");
  const locale = useLocale();
  const isAdmin = useIsAdmin();
  const queryClient = useQueryClient();
  const [connectUrl, setConnectUrl] = useState<string | null>(null);

  const calendars = useCalendarConnections();
  const rows = calendars.data?.pages.flatMap((p) => p.data) ?? [];
  const page = calendars.data?.pages.at(-1)?.page;
  const officeCalendarReady = rows.some(
    (row) => row.provider === "office_calendar" && row.status === "active"
  );

  const principals = usePrincipals();
  const names = useMemo(() => {
    const map = new Map<string, string>();
    for (const p of principals.data?.pages.flatMap((pg) => pg.data) ?? []) {
      const name = p.name?.trim() || p.display_name?.trim();
      if (name) map.set(p.id, name);
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

  const setupOfficeCalendar = useMutation({
    mutationFn: () =>
      createLocalCalendarConnection({ idempotencyKey: idempotencyKey() }),
    onSuccess: async () => {
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ["settings", "calendar-connections"] }),
        queryClient.invalidateQueries({ queryKey: ["meetings", "calendar-connections"] }),
        queryClient.invalidateQueries({ queryKey: ["meetings", "types"] }),
      ]);
      toast.success(t("officeSetupSuccess"));
    },
    onError: (err) => toast.error(apiErrorText(tSettings, err)),
  });

  const columns: ColumnDef<CalendarConnection>[] = [
    {
      key: "host",
      header: t("columns.host"),
      cell: (row) => (
        <span className="text-sm">
          {names.get(row.host_principal_id) ?? tc("nameUnavailable")}
        </span>
      ),
    },
    {
      key: "calendar",
      header: t("columns.calendar"),
      cell: (row) => (
        row.provider === "office_calendar" ? (
          <span className="font-mono text-xs">{t("officeCalendar")}</span>
        ) : (
          <div className="flex flex-col gap-1 text-xs">
            <span className="text-muted-foreground">{t("calendarNameUnavailable")}</span>
            <details>
              <summary className="cursor-pointer text-muted-foreground">{t("showCalendarId")}</summary>
              <code className="break-all">{row.provider_calendar_id}</code>
            </details>
          </div>
        )
      ),
    },
    {
      key: "provider",
      header: t("columns.provider"),
      cell: (row) => (
        <span className="text-xs">
          {row.provider === "office_calendar" ? t("officeProvider") : row.provider}
        </span>
      ),
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
      />

      <div className="grid gap-4 md:grid-cols-2">
        <section className="flex flex-col gap-3 rounded-xl border bg-card p-4">
          <div>
            <h2 className="text-sm font-semibold">{t("officeTitle")}</h2>
            <p className="mt-1 text-xs text-muted-foreground">{t("officeDescription")}</p>
            <p className="mt-2 text-xs text-muted-foreground">{t("officeSyncNotice")}</p>
          </div>
          {isAdmin ? (
            <Button
              type="button"
              size="sm"
              className="self-start"
              disabled={setupOfficeCalendar.isPending || officeCalendarReady}
              onClick={() => setupOfficeCalendar.mutate()}
            >
              {setupOfficeCalendar.isPending
                ? t("officeSettingUp")
                : officeCalendarReady
                  ? t("officeReady")
                  : t("officeSetup")}
            </Button>
          ) : officeCalendarReady ? (
            <Badge variant="info-subtle" className="self-start">{t("officeReady")}</Badge>
          ) : null}
        </section>

        <section className="flex flex-col gap-3 rounded-xl border bg-card p-4">
          <div>
            <h2 className="text-sm font-semibold">{t("zohoTitle")}</h2>
            <p className="mt-1 text-xs text-muted-foreground">{t("zohoDescription")}</p>
          </div>
          {isAdmin ? (
            <Button
              type="button"
              size="sm"
              className="self-start"
              disabled={connect.isPending}
              onClick={() => connect.mutate()}
            >
              {connect.isPending ? t("connectingZoho") : t("connectZoho")}
            </Button>
          ) : null}
        </section>
      </div>

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
