"use client";

import { useLocale, useTranslations } from "next-intl";
import Link from "next/link";
import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";

import { DataTable, type ColumnDef } from "@/components/patterns/data-table";
import { PageHeader } from "@/components/patterns/page-header";
import { StatusBadge } from "@/components/patterns/status-badge";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { getMe } from "@/lib/api/auth";
import { usePrincipals } from "@/lib/api/settings";
import {
  APPROVALS_PATH,
  type ApprovalListItem,
  type ApprovalStatus,
} from "@/lib/api/approvals";
import { useCursorList } from "@/lib/api/use-cursor-list";
import { cn } from "@/lib/utils";

import { formatDateTime } from "./format";
import { isSelfRequest } from "./rules";

const STATUSES: ApprovalStatus[] = [
  "pending",
  "approved",
  "rejected",
  "expired",
  "revoked",
];

const KINDS = [
  "campaign_launch",
  "message_submit",
  "booking_change",
  "suppression_release",
] as const;

function requesterName(
  row: ApprovalListItem,
  principalNames: Map<string, string>,
  unavailable: string
): string {
  return (
    row.requester_display_name?.trim() ||
    row.requester?.display_name?.trim() ||
    row.requester?.name?.trim() ||
    principalNames.get(row.requested_by) ||
    unavailable
  );
}

export function ApprovalList() {
  const t = useTranslations("approvals.list");
  const tKind = useTranslations("approvals.kind");
  const tc = useTranslations("common");
  const tStatus = useTranslations("status");
  const locale = useLocale();

  const [status, setStatus] = useState<string>("pending");
  const [kind, setKind] = useState<string>("all");
  const [mineOnly, setMineOnly] = useState(false);
  const principalsQuery = usePrincipals();
  const principalNames = useMemo(() => {
    const map = new Map<string, string>();
    for (const principal of principalsQuery.data?.pages.flatMap((page) => page.data) ?? []) {
      const name = principal.name?.trim() || principal.display_name?.trim();
      if (name) map.set(principal.id, name);
    }
    return map;
  }, [principalsQuery.data]);

  // "pending·종류별 목록"(B.2) — 상태·종류는 문서화된 필드명으로 서버에 넘긴다.
  const list = useCursorList<ApprovalListItem>({
    queryKey: ["approvals", "list", status, kind],
    path: APPROVALS_PATH,
    params: {
      status: status === "all" ? undefined : status,
      action_type: kind === "all" ? undefined : kind,
    },
  });

  const me = useQuery({
    queryKey: ["me"],
    queryFn: async () => (await getMe()).data,
    retry: false,
    staleTime: 60_000,
  });

  const rows = useMemo(() => {
    const all = list.data?.pages.flatMap((p) => p.data) ?? [];
    // "내가 요청한 것" 필터는 문서화 파라미터가 없어 클라이언트에서 처리한다.
    if (!mineOnly || !me.data?.principal_id) return all;
    return all.filter((r) => isSelfRequest(r, me.data.principal_id));
  }, [list.data, mineOnly, me.data]);

  const lastPage = list.data?.pages[list.data.pages.length - 1];

  const columns: ColumnDef<ApprovalListItem>[] = [
    {
      key: "kind",
      header: t("colKind"),
      cell: (row) => (
        <span className="flex items-center gap-2">
          <Badge variant="secondary">
            {tKind.has(row.action_type) ? tKind(row.action_type) : row.action_type}
          </Badge>
          {isSelfRequest(row, me.data?.principal_id) ? (
            <Badge variant="outline">{t("scopeMine")}</Badge>
          ) : null}
        </span>
      ),
    },
    {
      key: "requester",
      header: t("colRequester"),
      cell: (row) => (
        <Link
          href={`/app/approvals/${row.id}`}
          className="font-medium text-foreground underline-offset-4 hover:underline"
        >
          {requesterName(row, principalNames, tc("nameUnavailable"))}
        </Link>
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
    {
      key: "expires",
      header: t("colExpires"),
      className: "w-44",
      cell: (row) => (
        <span className="text-muted-foreground">
          {formatDateTime(row.expires_at, locale)}
        </span>
      ),
    },
    {
      key: "status",
      header: t("colStatus"),
      className: "w-36",
      cell: (row) => <StatusBadge status={row.status} />,
    },
  ];

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-6 py-6">
      <PageHeader title={t("title")} description={t("description")} />

      <div className="flex flex-wrap items-center gap-2">
        <div
          role="group"
          aria-label={t("filterStatus")}
          className="flex rounded-full border border-border p-0.5"
        >
          {(["all", "mine"] as const).map((scope) => (
            <Button
              key={scope}
              type="button"
              size="xs"
              variant={mineOnly === (scope === "mine") ? "default" : "ghost"}
              aria-pressed={mineOnly === (scope === "mine")}
              onClick={() => setMineOnly(scope === "mine")}
              className={cn("rounded-full")}
            >
              {scope === "mine" ? t("scopeMine") : t("scopeAll")}
            </Button>
          ))}
        </div>

        <Select value={status} onValueChange={setStatus}>
          <SelectTrigger size="sm" aria-label={t("filterStatus")} className="w-36">
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

        <Select value={kind} onValueChange={setKind}>
          <SelectTrigger size="sm" aria-label={t("filterKind")} className="w-40">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="all">{t("allKinds")}</SelectItem>
            {KINDS.map((k) => (
              <SelectItem key={k} value={k}>
                {tKind.has(k) ? tKind(k) : k}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>

      <DataTable
        columns={columns}
        rows={rows}
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
