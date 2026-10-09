"use client";

/**
 * UI13 연동 화면 — 메일 계정·발송 채널·캘린더의 연결 건강 + 정책 검토.
 * OAuth 비밀 값은 표시하지 않고, 재인증 필요는 경고 배너로 표면에 올린다.
 */
import { TriangleAlertIcon } from "lucide-react";
import { useLocale, useTranslations } from "next-intl";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useMemo, useRef, useState } from "react";
import { toast } from "sonner";

import { DataTable, type ColumnDef } from "@/components/patterns/data-table";
import { EmptyState } from "@/components/patterns/empty-state";
import { PageHeader } from "@/components/patterns/page-header";
import { StatusBadge } from "@/components/patterns/status-badge";
import { Alert, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { idempotencyKey } from "@/lib/api/client";
import { ApiError } from "@/lib/api/problem";
import {
  authorizationUrl,
  reconnectMailAccount,
  useCalendarConnections,
  useMailAccounts,
  usePrincipals,
  type CalendarConnection,
  type MailAccount,
} from "@/lib/api/settings";
import { ConnectUrlDialog } from "@/features/operations/connect-url-dialog";
import { DeliveryChannelsSection } from "@/features/operations/delivery-channels";
import { apiErrorText } from "@/features/settings/api-error";
import { formatDateTime } from "@/features/settings/format";
import { useIsAdmin } from "@/features/settings/use-me";

export function IntegrationsScreen() {
  const t = useTranslations("operations.integrations");
  const tOps = useTranslations("operations");
  const locale = useLocale();
  const isAdmin = useIsAdmin();
  const queryClient = useQueryClient();
  const [connectUrl, setConnectUrl] = useState<string | null>(null);

  const mailAccounts = useMailAccounts();
  const accounts = mailAccounts.data?.pages.flatMap((p) => p.data) ?? [];
  const accountsPage = mailAccounts.data?.pages.at(-1)?.page;

  const calendars = useCalendarConnections();
  const calendarRows = calendars.data?.pages.flatMap((p) => p.data) ?? [];
  const calendarsPage = calendars.data?.pages.at(-1)?.page;

  const principals = usePrincipals();
  const principalNames = useMemo(() => {
    const map = new Map<string, string>();
    for (const p of principals.data?.pages.flatMap((pg) => pg.data) ?? []) {
      map.set(p.id, p.name);
    }
    return map;
  }, [principals.data]);

  const reauthCount =
    accounts.filter((a) => a.status === "reauth_required").length +
    calendarRows.filter((c) => c.status === "reauth_required").length;

  // 재연결 시도당 멱등 키 하나 — 재시도 안전.
  const idemRef = useRef(idempotencyKey());
  const reconnect = useMutation({
    mutationFn: (id: string) =>
      reconnectMailAccount(id, { idempotencyKey: idemRef.current }),
    onSuccess: (res) => {
      const url = authorizationUrl(res.data);
      if (url) setConnectUrl(url);
      else toast.success(t("reconnectDone"));
      idemRef.current = idempotencyKey();
      queryClient.invalidateQueries({ queryKey: ["settings", "mail-accounts"] });
    },
    onError: (err) => {
      toast.error(apiErrorText(tOps, err));
      if (err instanceof ApiError && err.problem.retryable !== true && err.status < 500) {
        idemRef.current = idempotencyKey();
      }
    },
  });

  const accountColumns: ColumnDef<MailAccount>[] = [
    {
      key: "name",
      header: t("mailSection"),
      cell: (row) => (
        <div className="flex flex-col">
          <span className="font-medium">{row.name}</span>
          <span className="text-2xs text-muted-foreground">{row.primary_address}</span>
        </div>
      ),
    },
    {
      key: "provider",
      header: "",
      cell: (row) => (
        <span className="text-xs">
          {t.has(`provider.${row.provider}`) ? t(`provider.${row.provider}`) : row.provider}
        </span>
      ),
    },
    {
      key: "status",
      header: "",
      cell: (row) => <StatusBadge status={row.status} />,
    },
    {
      key: "lastSync",
      header: "",
      cell: (row) => (
        <div className="flex flex-col text-xs">
          <span className="whitespace-nowrap">
            {row.last_synced_at
              ? t("lastSync", { time: formatDateTime(row.last_synced_at, locale) })
              : t("neverSynced")}
          </span>
          {row.last_error_code ? (
            <span className="text-2xs text-destructive">
              {t("lastError", { code: row.last_error_code })}
            </span>
          ) : null}
        </div>
      ),
    },
    {
      key: "actions",
      header: "",
      cell: (row) =>
        isAdmin && row.status === "reauth_required" ? (
          <Button
            type="button"
            variant="outline"
            size="xs"
            disabled={reconnect.isPending}
            onClick={() => reconnect.mutate(row.id)}
          >
            {t("reconnect")}
          </Button>
        ) : null,
    },
  ];

  const calendarColumns: ColumnDef<CalendarConnection>[] = [
    {
      key: "host",
      header: t("calendar.host"),
      cell: (row) => (
        <span className="text-sm">
          {principalNames.get(row.host_principal_id) ?? row.host_principal_id.slice(0, 8)}
        </span>
      ),
    },
    {
      key: "calendar",
      header: t("calendar.calendar"),
      cell: (row) => <span className="font-mono text-xs">{row.provider_calendar_id}</span>,
    },
    {
      key: "status",
      header: t("calendar.status"),
      cell: (row) => <StatusBadge status={row.status} />,
    },
    {
      key: "busy",
      header: t("calendar.busy"),
      cell: (row) => (
        <Badge variant={row.include_in_busy ? "info-subtle" : "secondary"}>
          {row.include_in_busy ? t("calendar.busyYes") : t("calendar.busyNo")}
        </Badge>
      ),
    },
  ];

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-6 px-6 py-6">
      <PageHeader title={t("title")} description={t("description")} />

      <p className="text-2xs text-muted-foreground">{t("oauthNotice")}</p>

      {reauthCount > 0 ? (
        <Alert variant="warning">
          <TriangleAlertIcon />
          <AlertTitle>{tOps("alerts.reauth", { count: reauthCount })}</AlertTitle>
        </Alert>
      ) : null}

      <section className="flex flex-col gap-3" aria-labelledby="integrations-mail">
        <h2 id="integrations-mail" className="text-sm font-medium">
          {t("mailSection")}
        </h2>
        <DataTable
          columns={accountColumns}
          rows={accounts}
          rowKey={(row) => row.id}
          loading={mailAccounts.isPending}
          error={mailAccounts.error}
          onRetry={() => mailAccounts.refetch()}
          page={accountsPage}
          onLoadMore={() => mailAccounts.fetchNextPage()}
          loadingMore={mailAccounts.isFetchingNextPage}
          emptyState={
            <EmptyState title={t("emptyTitle")} description={t("emptyDescription")} compact />
          }
          caption={t("mailSection")}
        />
      </section>

      <DeliveryChannelsSection />

      <section className="flex flex-col gap-3" aria-labelledby="integrations-calendars">
        <h2 id="integrations-calendars" className="text-sm font-medium">
          {t("calendarSection")}
        </h2>
        <DataTable
          columns={calendarColumns}
          rows={calendarRows}
          rowKey={(row) => row.id}
          loading={calendars.isPending}
          error={calendars.error}
          onRetry={() => calendars.refetch()}
          page={calendarsPage}
          onLoadMore={() => calendars.fetchNextPage()}
          loadingMore={calendars.isFetchingNextPage}
          emptyState={
            <EmptyState
              title={t("calendar.emptyTitle")}
              description={t("calendar.emptyDescription")}
              compact
            />
          }
          caption={t("calendarSection")}
        />
      </section>

      <ConnectUrlDialog
        url={connectUrl}
        open={connectUrl !== null}
        onOpenChange={(open) => !open && setConnectUrl(null)}
      />
    </div>
  );
}
