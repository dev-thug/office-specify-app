"use client";

import { useLocale, useTranslations } from "next-intl";
import Link from "next/link";
import { useMemo, useState } from "react";
import { PlusIcon, SearchIcon } from "lucide-react";

import { DataTable, type ColumnDef } from "@/components/patterns/data-table";
import { PageHeader } from "@/components/patterns/page-header";
import { StatusBadge } from "@/components/patterns/status-badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  CAMPAIGNS_PATH,
  type CampaignListItem,
  type CampaignStatus,
} from "@/lib/api/campaigns";
import { useCursorList } from "@/lib/api/use-cursor-list";
import { useMailAccounts } from "@/lib/api/settings";

import { formatDateTime } from "./format";

const STATUSES: CampaignStatus[] = [
  "draft",
  "awaiting_approval",
  "active",
  "paused",
  "completed",
  "canceled",
];

export function CampaignList() {
  const t = useTranslations("campaigns.list");
  const tCommon = useTranslations("common");
  const tStatus = useTranslations("status");
  const locale = useLocale();
  const [query, setQuery] = useState("");
  const [status, setStatus] = useState<string>("all");
  const mailAccounts = useMailAccounts();
  const mailboxNames = useMemo(() => {
    const names = new Map<string, string>();
    for (const account of mailAccounts.data?.pages.flatMap((page) => page.data) ?? []) {
      const label = [account.name?.trim(), account.primary_address?.trim()].filter(Boolean).join(" · ");
      if (label) names.set(account.id, label);
    }
    return names;
  }, [mailAccounts.data]);

  const list = useCursorList<CampaignListItem>({
    queryKey: ["campaigns", "list"],
    path: CAMPAIGNS_PATH,
  });

  const rows = useMemo(
    () => list.data?.pages.flatMap((p) => p.data) ?? [],
    [list.data]
  );

  // 검색·상태 필터는 불러온 페이지 범위에서 클라이언트가 처리한다
  // (B.2에 캠페인 검색/필터 파라미터가 문서화되어 있지 않다).
  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    return rows.filter((r) => {
      if (status !== "all" && r.status !== status) return false;
      if (!q) return true;
      return (
        r.name.toLowerCase().includes(q) ||
        (r.objective ?? "").toLowerCase().includes(q)
      );
    });
  }, [rows, query, status]);

  const lastPage = list.data?.pages[list.data.pages.length - 1];

  const columns: ColumnDef<CampaignListItem>[] = [
    {
      key: "name",
      header: t("colName"),
      cell: (row) => (
        <span className="flex min-w-0 flex-col">
          <Link
            href={`/app/campaigns/${row.id}`}
            className="truncate font-medium text-foreground underline-offset-4 hover:underline"
          >
            {row.name}
          </Link>
          {row.objective ? (
            <span className="truncate text-2xs text-muted-foreground">
              {row.objective}
            </span>
          ) : null}
        </span>
      ),
    },
    {
      key: "status",
      header: t("colStatus"),
      className: "w-36",
      cell: (row) => <StatusBadge status={row.status} />,
    },
    {
      key: "mailbox",
      header: t("colMailbox"),
      className: "w-40",
      cell: (row) => (
        <span className="text-2xs text-muted-foreground">
          {mailboxNames.get(row.mail_account_id) ?? tCommon("nameUnavailable")}
        </span>
      ),
    },
    {
      key: "created",
      header: t("colCreated"),
      className: "w-44",
      cell: (row) => (
        <span className="text-muted-foreground">
          {formatDateTime(row.created_at, locale)}
        </span>
      ),
    },
  ];

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-6 py-6">
      <PageHeader
        title={t("title")}
        description={t("description")}
        actions={
          <Button asChild size="sm">
            <Link href="/app/campaigns/new">
              <PlusIcon aria-hidden />
              {t("newCampaign")}
            </Link>
          </Button>
        }
      />

      <div className="flex flex-wrap items-center gap-2">
        <div className="relative max-w-sm flex-1">
          <SearchIcon
            aria-hidden
            className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground"
          />
          <Input
            type="search"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={t("searchPlaceholder")}
            aria-label={t("searchPlaceholder")}
            className="pl-9"
          />
        </div>
        <Select value={status} onValueChange={setStatus}>
          <SelectTrigger
            size="sm"
            aria-label={t("filterStatus")}
            className="w-36"
          >
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="all">{t("allStatuses")}</SelectItem>
            {STATUSES.map((s) => (
              <SelectItem key={s} value={s}>
                {tStatus.has(s) ? tStatus(s) : s}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>

      <DataTable
        columns={columns}
        rows={filtered}
        rowKey={(row) => row.id}
        page={lastPage?.page}
        onLoadMore={() => list.fetchNextPage()}
        loadingMore={list.isFetchingNextPage}
        loading={list.isLoading}
        error={list.error}
        onRetry={() => list.refetch()}
        emptyState={
          <div className="flex flex-col items-center gap-1 py-8 text-center">
            <p className="text-sm font-medium">{t("emptyTitle")}</p>
            <p className="max-w-md text-sm text-muted-foreground">
              {t("emptyDescription")}
            </p>
          </div>
        }
      />
    </div>
  );
}
