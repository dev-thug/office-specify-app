"use client";

/**
 * 설정 > 정책 — 게이트 상태 읽기 전용 뷰.
 * 발송 채널의 policy_status·연결 상태·긴급 중지를 표시만 한다 —
 * 변경은 각 관리 화면(운영 > 연동)에서 서버 검증과 함께 이뤄진다.
 * UI 체크박스는 법적 동의의 근거가 아니라는 안내를 함께 둔다 (§7.2).
 */
import Link from "next/link";
import { useLocale, useTranslations } from "next-intl";
import { useQuery } from "@tanstack/react-query";
import { OctagonXIcon, ShieldCheckIcon } from "lucide-react";

import { DataTable, type ColumnDef } from "@/components/patterns/data-table";
import { EmptyState } from "@/components/patterns/empty-state";
import { PageHeader } from "@/components/patterns/page-header";
import { StatusBadge } from "@/components/patterns/status-badge";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { getEmergencyStop } from "@/lib/api/operations";
import {
  useDeliveryChannels,
  useMailAccounts,
  type DeliveryChannel,
  type MailAccount,
} from "@/lib/api/settings";
import { formatDateTime } from "@/features/settings/format";

export function PoliciesScreen() {
  const t = useTranslations("settings.policies");
  const locale = useLocale();

  const stop = useQuery({
    queryKey: ["operations", "emergency-stop"],
    queryFn: getEmergencyStop,
    retry: false,
  });
  const stopped = stop.data?.data.enabled === true;

  const channels = useDeliveryChannels();
  const channelRows = channels.data?.pages.flatMap((p) => p.data) ?? [];
  const channelsPage = channels.data?.pages.at(-1)?.page;

  const accounts = useMailAccounts();
  const accountRows = accounts.data?.pages.flatMap((p) => p.data) ?? [];
  const accountsPage = accounts.data?.pages.at(-1)?.page;

  const channelColumns: ColumnDef<DeliveryChannel>[] = [
    {
      key: "name",
      header: t("channels.columns.name"),
      cell: (row) => <span className="font-medium">{row.name}</span>,
    },
    {
      key: "purpose",
      header: t("channels.columns.purpose"),
      cell: (row) => <span className="text-xs">{row.purpose}</span>,
    },
    {
      key: "policy",
      header: t("channels.columns.policy"),
      cell: (row) => <StatusBadge status={row.policy_status} />,
    },
    {
      key: "enabled",
      header: t("channels.columns.enabled"),
      cell: (row) => (
        <Badge variant={row.enabled ? "success-subtle" : "secondary"}>
          {row.enabled ? t("channels.enabledYes") : t("channels.enabledNo")}
        </Badge>
      ),
    },
  ];

  const accountColumns: ColumnDef<MailAccount>[] = [
    {
      key: "address",
      header: t("accounts.columns.address"),
      cell: (row) => <span className="font-mono text-xs">{row.primary_address}</span>,
    },
    {
      key: "status",
      header: t("accounts.columns.status"),
      cell: (row) => <StatusBadge status={row.status} />,
    },
    {
      key: "lastSync",
      header: t("accounts.columns.lastSync"),
      cell: (row) => (
        <span className="whitespace-nowrap text-xs">
          {formatDateTime(row.last_synced_at, locale)}
        </span>
      ),
    },
  ];

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-6 px-6 py-6">
      <PageHeader title={t("title")} description={t("description")} />

      <Alert variant={stopped ? "warning" : "info"}>
        {stopped ? <OctagonXIcon /> : <ShieldCheckIcon />}
        <AlertTitle>
          {stopped ? t("emergency.on") : t("emergency.off")}
        </AlertTitle>
        <AlertDescription>
          {t("emergency.description")}
          {stop.data?.data.reason ? ` — ${stop.data.data.reason}` : ""}
        </AlertDescription>
      </Alert>

      <p className="text-2xs text-muted-foreground">{t("consentNotice")}</p>

      <section className="flex flex-col gap-3" aria-labelledby="policy-channels">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <h2 id="policy-channels" className="text-sm font-medium">
            {t("channels.title")}
          </h2>
          <Link
            href="/app/operations/integrations"
            className="text-xs font-medium text-primary underline-offset-4 hover:underline"
          >
            {t("manageLink")}
          </Link>
        </div>
        <DataTable
          columns={channelColumns}
          rows={channelRows}
          rowKey={(row) => row.id}
          loading={channels.isPending}
          error={channels.error}
          onRetry={() => channels.refetch()}
          page={channelsPage}
          onLoadMore={() => channels.fetchNextPage()}
          loadingMore={channels.isFetchingNextPage}
          emptyState={
            <EmptyState
              title={t("channels.emptyTitle")}
              description={t("channels.emptyDescription")}
            />
          }
          caption={t("channels.title")}
        />
      </section>

      <section className="flex flex-col gap-3" aria-labelledby="policy-accounts">
        <h2 id="policy-accounts" className="text-sm font-medium">
          {t("accounts.title")}
        </h2>
        <DataTable
          columns={accountColumns}
          rows={accountRows}
          rowKey={(row) => row.id}
          loading={accounts.isPending}
          error={accounts.error}
          onRetry={() => accounts.refetch()}
          page={accountsPage}
          onLoadMore={() => accounts.fetchNextPage()}
          loadingMore={accounts.isFetchingNextPage}
          emptyState={
            <EmptyState
              title={t("accounts.emptyTitle")}
              description={t("accounts.emptyDescription")}
            />
          }
          caption={t("accounts.title")}
        />
      </section>
    </div>
  );
}
