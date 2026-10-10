"use client";

/**
 * 감사 로그 뷰 (GET /audit-logs — 제한된 감사 권한, redacted).
 * details 는 허용 목록 메타데이터만 담긴다 — 원문·토큰은 서버가 제외한다.
 */
import { useLocale, useTranslations } from "next-intl";
import { useState } from "react";

import { DataTable, type ColumnDef } from "@/components/patterns/data-table";
import { EmptyState } from "@/components/patterns/empty-state";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { useAuditLogs, type AuditLogEntry } from "@/lib/api/operations";
import { usePrincipals } from "@/lib/api/settings";
import { formatDateTime } from "@/features/settings/format";

export function AuditLogTable() {
  const t = useTranslations("operations.audit");
  const tJobs = useTranslations("operations.jobs");
  const tc = useTranslations("common");
  const locale = useLocale();
  const principals = usePrincipals();
  const [action, setAction] = useState("");
  const [targetType, setTargetType] = useState("");
  const [selected, setSelected] = useState<AuditLogEntry | null>(null);

  const list = useAuditLogs({
    action: action || undefined,
    target_type: targetType || undefined,
  });
  const rows = list.data?.pages.flatMap((p) => p.data) ?? [];
  const page = list.data?.pages.at(-1)?.page;

  const columns: ColumnDef<AuditLogEntry>[] = [
    {
      key: "occurredAt",
      header: t("columns.occurredAt"),
      cell: (row) => (
        <span className="whitespace-nowrap text-xs">
          {formatDateTime(row.occurred_at, locale)}
        </span>
      ),
    },
    {
      key: "action",
      header: t("columns.action"),
      cell: (row) => <span className="font-mono text-xs">{row.action}</span>,
    },
    {
      key: "actor",
      header: t("columns.actor"),
      cell: (row) => {
        if (!row.actor_principal_id) return <span>{t("systemActor")}</span>;
        const principal = principals.data?.pages
          .flatMap((page) => page.data)
          .find((item) => item.id === row.actor_principal_id);
        return <span>{principal?.name?.trim() || principal?.display_name?.trim() || tc("nameUnavailable")}</span>;
      },
    },
    {
      key: "targetType",
      header: t("columns.targetType"),
      cell: (row) => <span className="font-mono text-xs">{row.target_type}</span>,
    },
    {
      key: "requestId",
      header: t("columns.requestId"),
      cell: (row) => <span className="font-mono text-xs">{row.request_id}</span>,
    },
    {
      key: "actions",
      header: "",
      cell: (row) => (
        <Button
          type="button"
          variant="ghost"
          size="xs"
          onClick={() => setSelected(row)}
        >
          {tJobs("openDetail")}
        </Button>
      ),
    },
  ];

  return (
    <div className="flex flex-col gap-3">
      <p className="text-2xs text-muted-foreground">{t("description")}</p>
      <div className="flex flex-wrap items-center gap-2">
        <Input
          value={action}
          onChange={(e) => setAction(e.target.value)}
          placeholder={t("filterActionPlaceholder")}
          aria-label={t("filterAction")}
          className="w-56"
        />
        <Input
          value={targetType}
          onChange={(e) => setTargetType(e.target.value)}
          placeholder={t("filterTargetPlaceholder")}
          aria-label={t("filterTarget")}
          className="w-56"
        />
      </div>
      <DataTable
        columns={columns}
        rows={rows}
        rowKey={(row) => row.id}
        loading={list.isPending}
        error={list.error}
        onRetry={() => list.refetch()}
        page={page}
        onLoadMore={() => list.fetchNextPage()}
        loadingMore={list.isFetchingNextPage}
        emptyState={
          <EmptyState title={t("emptyTitle")} description={t("emptyDescription")} />
        }
        caption={t("columns.action")}
      />
      <AuditDetailDialog
        entry={selected}
        onOpenChange={(open) => !open && setSelected(null)}
      />
    </div>
  );
}

function AuditDetailDialog({
  entry,
  onOpenChange,
}: {
  entry: AuditLogEntry | null;
  onOpenChange: (open: boolean) => void;
}) {
  const t = useTranslations("operations.audit");
  return (
    <Dialog open={!!entry} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle className="font-mono text-sm">{entry?.action}</DialogTitle>
          <DialogDescription className="font-mono text-xs">
            {entry ? t("auditEventId", { id: entry.id }) : ""}
          </DialogDescription>
        </DialogHeader>
        <div className="flex flex-col gap-2">
          <p className="text-xs font-medium text-muted-foreground">{t("details")}</p>
          <pre className="max-h-72 overflow-auto rounded-2xl border border-border bg-app-panel-muted p-3 text-xs">
            {entry ? JSON.stringify(entry.details, null, 2) : ""}
          </pre>
        </div>
      </DialogContent>
    </Dialog>
  );
}
