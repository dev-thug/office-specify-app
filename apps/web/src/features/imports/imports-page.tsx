"use client";

import { useFormatter, useTranslations } from "next-intl";
import Link from "next/link";
import { useState } from "react";

import { DataTable, type ColumnDef } from "@/components/patterns/data-table";
import { EmptyState } from "@/components/patterns/empty-state";
import { PageHeader } from "@/components/patterns/page-header";
import { StatusBadge } from "@/components/patterns/status-badge";
import { Button } from "@/components/ui/button";
import {
  importJobId,
  useImportJobs,
  type ImportJob,
} from "@/lib/api/imports";

import { ExportDialog } from "./export-dialog";
import { ImportWizard } from "./import-wizard";

/** UI11 가져오기 목록 — 새 가져오기 마법사·보내기 요청·검증 상태. */
export function ImportsPage() {
  const t = useTranslations("imports.list");
  const tScreens = useTranslations("screens");
  const [wizardOpen, setWizardOpen] = useState(false);
  const [exportOpen, setExportOpen] = useState(false);

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-6 py-6">
      <PageHeader
        title={tScreens("imports.title")}
        description={tScreens("imports.description")}
        actions={
          <div className="flex items-center gap-2">
            <Button
              variant="outline"
              size="sm"
              onClick={() => setExportOpen(true)}
            >
              {t("export")}
            </Button>
            <Button size="sm" onClick={() => setWizardOpen(true)}>
              {t("new")}
            </Button>
          </div>
        }
      />
      <ImportsTable />
      <ImportWizard open={wizardOpen} onOpenChange={setWizardOpen} />
      <ExportDialog open={exportOpen} onOpenChange={setExportOpen} />
    </div>
  );
}

function ImportsTable() {
  const t = useTranslations("imports.list");
  const format = useFormatter();
  const query = useImportJobs();
  const rows = query.data?.pages.flatMap((p) => p.data) ?? [];
  const last = query.data?.pages.at(-1);

  const columns: ColumnDef<ImportJob>[] = [
    {
      key: "kind",
      header: t("columns.type"),
      cell: (job) => (
        <span>{job.kind === "export" ? t("typeExport") : t("typeImport")}</span>
      ),
    },
    {
      key: "status",
      header: t("columns.status"),
      cell: (job) => <StatusBadge status={job.status} />,
    },
    {
      key: "summary",
      header: t("columns.summary"),
      cell: (job) => {
        const s = job.summary ?? {};
        return typeof s.total_rows === "number" ? (
          <span className="text-muted-foreground">
            {t("summaryFormat", {
              total: s.total_rows,
              valid: s.valid_rows ?? 0,
              duplicate: s.duplicate_rows ?? 0,
              invalid: s.invalid_rows ?? 0,
            })}
          </span>
        ) : (
          <span className="text-muted-foreground">{t("noSummary")}</span>
        );
      },
    },
    {
      key: "createdAt",
      header: t("columns.createdAt"),
      cell: (job) => (
        <span className="text-muted-foreground">
          {format.dateTime(new Date(job.created_at), {
            dateStyle: "medium",
            timeStyle: "short",
          })}
        </span>
      ),
    },
    {
      key: "actions",
      header: t("columns.actions"),
      cell: (job) => (
        <Button variant="outline" size="xs" asChild>
          <Link href={`/app/imports/${importJobId(job)}`}>{t("detail")}</Link>
        </Button>
      ),
    },
  ];

  return (
    <DataTable
      columns={columns}
      rows={rows}
      rowKey={(job) => importJobId(job)}
      page={last?.page}
      onLoadMore={() => query.fetchNextPage()}
      loadingMore={query.isFetchingNextPage}
      loading={query.isPending}
      error={query.error}
      onRetry={() => query.refetch()}
      emptyState={
        <EmptyState
          title={t("empty")}
          description={t("emptyDescription")}
          compact
        />
      }
    />
  );
}
