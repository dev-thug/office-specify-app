"use client";

import { useQueryClient } from "@tanstack/react-query";
import { BanIcon, SearchIcon, XIcon } from "lucide-react";
import { useFormatter, useTranslations } from "next-intl";
import { useMemo, useState, type FormEvent } from "react";

import { DataTable, type ColumnDef } from "@/components/patterns/data-table";
import { EmptyState } from "@/components/patterns/empty-state";
import { PageHeader } from "@/components/patterns/page-header";
import { StatusBadge } from "@/components/patterns/status-badge";
import { Badge } from "@/components/ui/badge";
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
  SUPPRESSIONS_PATH,
  type Suppression,
  type SuppressionListParams,
  type SuppressionPurposeScope,
  type SuppressionReason,
} from "@/lib/api/compliance";
import { useCursorList } from "@/lib/api/use-cursor-list";
import { canRequestRelease } from "./allowed-actions";
import { SuppressionAddDialog } from "./suppression-add-dialog";
import { SuppressionReleaseDialog } from "./suppression-release-dialog";

type StateFilter = "active" | "released" | "";

/** UI12 "수신 거부·차단" — 주소 검색, 사유·범위별 차단 목록, 추가·해제 요청. */
export function SuppressionsScreen() {
  const t = useTranslations("compliance.suppressions");
  const format = useFormatter();
  const queryClient = useQueryClient();

  // 검색어는 제출 시에만 쿼리에 반영한다 — 타이핑마다 조회하지 않는다.
  const [qInput, setQInput] = useState("");
  const [q, setQ] = useState("");
  const [reason, setReason] = useState<SuppressionReason | "">("");
  const [scope, setScope] = useState<SuppressionPurposeScope | "">("");
  const [state, setState] = useState<StateFilter>("active");

  const params = useMemo<SuppressionListParams>(
    () => ({
      q: q || undefined,
      reason: reason || undefined,
      purpose_scope: scope || undefined,
      released: state === "" ? undefined : state === "released",
    }),
    [q, reason, scope, state]
  );

  const list = useCursorList<Suppression>({
    queryKey: ["compliance", "suppressions", params],
    path: SUPPRESSIONS_PATH,
    params,
  });

  const rows = useMemo(
    () => list.data?.pages.flatMap((p) => p.data) ?? [],
    [list.data]
  );
  const lastPage = list.data?.pages.at(-1)?.page;

  const [addOpen, setAddOpen] = useState(false);
  const [releaseTarget, setReleaseTarget] = useState<Suppression | null>(null);

  const invalidate = () =>
    queryClient.invalidateQueries({
      queryKey: ["compliance", "suppressions"],
    });

  const submitSearch = (e: FormEvent) => {
    e.preventDefault();
    setQ(qInput.trim());
  };

  const columns: ColumnDef<Suppression>[] = [
    {
      key: "address",
      header: t("colAddress"),
      cell: (row) => (
        <span className="font-medium">
          {row.email_address ?? row.email_address_id}
        </span>
      ),
    },
    {
      key: "scope",
      header: t("colScope"),
      cell: (row) => (
        <Badge variant="secondary">
          {t.has(`scope.${row.purpose_scope}`)
            ? t(`scope.${row.purpose_scope}`)
            : row.purpose_scope}
        </Badge>
      ),
    },
    {
      key: "reason",
      header: t("colReason"),
      cell: (row) =>
        t.has(`reason.${row.reason}`)
          ? t(`reason.${row.reason}`)
          : row.reason,
    },
    {
      key: "status",
      header: t("colStatus"),
      cell: (row) =>
        row.released_at === null ? (
          <StatusBadge status="blocked" label={t("stateActive")} />
        ) : (
          <StatusBadge status="released" label={t("stateReleased")} />
        ),
    },
    {
      key: "created_at",
      header: t("colCreatedAt"),
      cell: (row) => (
        <span className="text-muted-foreground">
          {format.dateTime(new Date(row.created_at), {
            dateStyle: "medium",
            timeStyle: "short",
          })}
        </span>
      ),
    },
    {
      key: "released_at",
      header: t("colReleasedAt"),
      cell: (row) =>
        row.released_at ? (
          <span className="text-muted-foreground">
            {format.dateTime(new Date(row.released_at), {
              dateStyle: "medium",
              timeStyle: "short",
            })}
          </span>
        ) : (
          <span className="text-muted-foreground">—</span>
        ),
    },
    {
      key: "actions",
      header: t("colActions"),
      className: "text-right",
      cell: (row) => {
        return (
          <div className="flex flex-col items-end gap-1">
            {canRequestRelease(row) ? (
              <Button
                type="button"
                variant="outline"
                size="xs"
                onClick={() => setReleaseTarget(row)}
              >
                {t("releaseButton")}
              </Button>
            ) : null}
          </div>
        );
      },
    },
  ];

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-6 py-6">
      <PageHeader
        title={t("title")}
        description={t("description")}
        actions={
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => setAddOpen(true)}
          >
            <BanIcon aria-hidden className="size-4" />
            {t("addButton")}
          </Button>
        }
      />

      {/* 주소 검색 + 필터 (§7.2 검색·필터) */}
      <div className="flex flex-wrap items-center gap-2">
        <form
          onSubmit={submitSearch}
          className="flex min-w-56 flex-1 items-center gap-2 sm:max-w-sm"
          role="search"
        >
          <label htmlFor="suppression-search" className="sr-only">
            {t("searchLabel")}
          </label>
          <Input
            id="suppression-search"
            value={qInput}
            onChange={(e) => setQInput(e.target.value)}
            placeholder={t("searchPlaceholder")}
            className="flex-1"
          />
          {q ? (
            <Button
              type="button"
              variant="ghost"
              size="icon-sm"
              aria-label={t("searchClear")}
              onClick={() => {
                setQInput("");
                setQ("");
              }}
            >
              <XIcon aria-hidden className="size-4" />
            </Button>
          ) : null}
          <Button type="submit" variant="outline" size="sm">
            <SearchIcon aria-hidden className="size-4" />
            {t("searchSubmit")}
          </Button>
        </form>

        <Select
          value={reason || "__all__"}
          onValueChange={(v) =>
            setReason(v === "__all__" ? "" : (v as SuppressionReason))
          }
        >
          <SelectTrigger size="sm" aria-label={t("filterReason")}>
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="__all__">
              {t("filterReason")}: {t("filterAll")}
            </SelectItem>
            <SelectItem value="unsubscribe">
              {t("reason.unsubscribe")}
            </SelectItem>
            <SelectItem value="complaint">{t("reason.complaint")}</SelectItem>
            <SelectItem value="hard_bounce">
              {t("reason.hard_bounce")}
            </SelectItem>
            <SelectItem value="manual">{t("reason.manual")}</SelectItem>
          </SelectContent>
        </Select>

        <Select
          value={scope || "__all__"}
          onValueChange={(v) =>
            setScope(v === "__all__" ? "" : (v as SuppressionPurposeScope))
          }
        >
          <SelectTrigger size="sm" aria-label={t("filterScope")}>
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="__all__">
              {t("filterScope")}: {t("filterAll")}
            </SelectItem>
            <SelectItem value="marketing">{t("scope.marketing")}</SelectItem>
            <SelectItem value="all">{t("scope.all")}</SelectItem>
          </SelectContent>
        </Select>

        <Select
          value={state || "__all__"}
          onValueChange={(v) => setState(v === "__all__" ? "" : (v as StateFilter))}
        >
          <SelectTrigger size="sm" aria-label={t("filterState")}>
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="__all__">
              {t("filterState")}: {t("filterAll")}
            </SelectItem>
            <SelectItem value="active">{t("stateActive")}</SelectItem>
            <SelectItem value="released">{t("stateReleased")}</SelectItem>
          </SelectContent>
        </Select>
      </div>

      <DataTable<Suppression>
        columns={columns}
        rows={rows}
        rowKey={(row) => row.id}
        page={lastPage}
        onLoadMore={() => list.fetchNextPage()}
        loadingMore={list.isFetchingNextPage}
        loading={list.isPending}
        error={list.error}
        onRetry={() => list.refetch()}
        caption={t("title")}
        emptyState={
          <EmptyState
            title={q ? t("emptySearchTitle") : t("emptyTitle")}
            description={t("emptyDescription")}
          />
        }
      />

      <SuppressionAddDialog
        open={addOpen}
        onOpenChange={setAddOpen}
        onCreated={invalidate}
      />
      <SuppressionReleaseDialog
        suppression={releaseTarget}
        onOpenChange={(open) => {
          if (!open) setReleaseTarget(null);
        }}
        onRequested={invalidate}
      />
    </div>
  );
}
