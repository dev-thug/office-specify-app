"use client";

import { useQueryClient } from "@tanstack/react-query";
import { SearchIcon, XIcon } from "lucide-react";
import { useFormatter, useTranslations } from "next-intl";
import { useMemo, useState, type FormEvent } from "react";

import { DataTable, type ColumnDef } from "@/components/patterns/data-table";
import { EmptyState } from "@/components/patterns/empty-state";
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
  COMMUNICATION_BASES_PATH,
  type BasisPurpose,
  type BasisStatus,
  type CommunicationBasis,
  type CommunicationBasisListParams,
} from "@/lib/api/compliance";
import { useCursorList } from "@/lib/api/use-cursor-list";
import { canReviewBasis, canRevokeBasis } from "./allowed-actions";
import { BasisReviewDialog } from "./basis-review-dialog";
import { BasisRevokeDialog } from "./basis-revoke-dialog";

/**
 * UI12 "근거 검토" — communication_bases 목록과 검토·철회.
 * 기본 필터는 pending(검토 대기) — 새 근거는 사람이 검토하기 전 valid가 아니다
 * (§11.4, DDL CHECK가 valid⇒reviewed_by/at을 강제).
 */
export function ReviewsScreen() {
  const t = useTranslations("compliance.reviews");
  const ts = useTranslations("status");
  const format = useFormatter();
  const queryClient = useQueryClient();

  const [qInput, setQInput] = useState("");
  const [q, setQ] = useState("");
  const [status, setStatus] = useState<BasisStatus | "">("pending");
  const [purpose, setPurpose] = useState<BasisPurpose | "">("");

  const params = useMemo<CommunicationBasisListParams>(
    () => ({
      q: q || undefined,
      status: status || undefined,
      purpose: purpose || undefined,
    }),
    [q, status, purpose]
  );

  const list = useCursorList<CommunicationBasis>({
    queryKey: ["compliance", "communication-bases", params],
    path: COMMUNICATION_BASES_PATH,
    params,
  });

  const rows = useMemo(
    () => list.data?.pages.flatMap((p) => p.data) ?? [],
    [list.data]
  );
  const lastPage = list.data?.pages.at(-1)?.page;

  const [reviewTarget, setReviewTarget] = useState<CommunicationBasis | null>(
    null
  );
  const [revokeTarget, setRevokeTarget] = useState<CommunicationBasis | null>(
    null
  );

  const invalidate = () =>
    queryClient.invalidateQueries({
      queryKey: ["compliance", "communication-bases"],
    });

  const submitSearch = (e: FormEvent) => {
    e.preventDefault();
    setQ(qInput.trim());
  };

  const columns: ColumnDef<CommunicationBasis>[] = [
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
      key: "purpose",
      header: t("colPurpose"),
      cell: (row) =>
        t.has(`purpose.${row.purpose}`)
          ? t(`purpose.${row.purpose}`)
          : row.purpose,
    },
    {
      key: "basis_type",
      header: t("colBasisType"),
      cell: (row) =>
        t.has(`basisType.${row.basis_type}`)
          ? t(`basisType.${row.basis_type}`)
          : row.basis_type,
    },
    {
      key: "jurisdiction",
      header: t("colJurisdiction"),
      cell: (row) => row.jurisdiction,
    },
    {
      key: "evidence",
      header: t("colEvidence"),
      cell: (row) => (
        <span
          className="block max-w-48 truncate font-mono text-2xs text-muted-foreground"
          title={row.evidence_ref}
        >
          {row.evidence_ref}
        </span>
      ),
    },
    {
      key: "collected_at",
      header: t("colCollectedAt"),
      cell: (row) => (
        <span className="text-muted-foreground">
          {format.dateTime(new Date(row.collected_at), {
            dateStyle: "medium",
          })}
        </span>
      ),
    },
    {
      key: "status",
      header: t("colStatus"),
      cell: (row) => <StatusBadge status={row.status} />,
    },
    {
      key: "valid_until",
      header: t("colValidUntil"),
      cell: (row) =>
        row.valid_until ? (
          <span className="text-muted-foreground">
            {format.dateTime(new Date(row.valid_until), {
              dateStyle: "medium",
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
      cell: (row) => (
        <span className="inline-flex gap-1.5">
          {canReviewBasis(row) ? (
            <Button
              type="button"
              variant="outline"
              size="xs"
              onClick={() => setReviewTarget(row)}
            >
              {t("reviewButton")}
            </Button>
          ) : null}
          {canRevokeBasis(row) ? (
            <Button
              type="button"
              variant="outline"
              size="xs"
              onClick={() => setRevokeTarget(row)}
            >
              {t("revokeButton")}
            </Button>
          ) : null}
        </span>
      ),
    },
  ];

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-6 py-6">
      <PageHeader title={t("title")} description={t("description")} />

      <div className="flex flex-wrap items-center gap-2">
        <form
          onSubmit={submitSearch}
          className="flex min-w-56 flex-1 items-center gap-2 sm:max-w-sm"
          role="search"
        >
          <label htmlFor="basis-search" className="sr-only">
            {t("searchLabel")}
          </label>
          <Input
            id="basis-search"
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
          value={status || "__all__"}
          onValueChange={(v) =>
            setStatus(v === "__all__" ? "" : (v as BasisStatus))
          }
        >
          <SelectTrigger size="sm" aria-label={t("filterStatus")}>
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="__all__">
              {t("filterStatus")}: {t("filterAll")}
            </SelectItem>
            {(
              ["pending", "valid", "rejected", "revoked", "expired"] as const
            ).map((s) => (
              <SelectItem key={s} value={s}>
                {ts(s)}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>

        <Select
          value={purpose || "__all__"}
          onValueChange={(v) =>
            setPurpose(v === "__all__" ? "" : (v as BasisPurpose))
          }
        >
          <SelectTrigger size="sm" aria-label={t("filterPurpose")}>
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="__all__">
              {t("filterPurpose")}: {t("filterAll")}
            </SelectItem>
            <SelectItem value="marketing">
              {t("purpose.marketing")}
            </SelectItem>
            <SelectItem value="correspondence">
              {t("purpose.correspondence")}
            </SelectItem>
            <SelectItem value="transactional">
              {t("purpose.transactional")}
            </SelectItem>
          </SelectContent>
        </Select>
      </div>

      <DataTable<CommunicationBasis>
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
            title={
              status === "pending" ? t("emptyPendingTitle") : t("emptyTitle")
            }
            description={t("emptyDescription")}
          />
        }
      />

      <BasisReviewDialog
        basis={reviewTarget}
        onOpenChange={(open) => {
          if (!open) setReviewTarget(null);
        }}
        onSettled={invalidate}
      />
      <BasisRevokeDialog
        basis={revokeTarget}
        onOpenChange={(open) => {
          if (!open) setRevokeTarget(null);
        }}
        onSettled={invalidate}
      />
    </div>
  );
}
