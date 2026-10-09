"use client";

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useTranslations } from "next-intl";
import { useRef, useState } from "react";
import { toast } from "sonner";

import { DataTable, type ColumnDef } from "@/components/patterns/data-table";
import { EmptyState } from "@/components/patterns/empty-state";
import { ErrorState } from "@/components/patterns/error-state";
import { PageHeader } from "@/components/patterns/page-header";
import { StatusBadge } from "@/components/patterns/status-badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { idempotencyKey } from "@/lib/api/client";
import { ApiError } from "@/lib/api/problem";
import {
  commitImport,
  getImport,
  getImportRowsPage,
  importJobId,
  useImportRows,
  type ImportJob,
  type ImportRow,
  type ImportRowStatus,
} from "@/lib/api/imports";

import { ImportWizard } from "./import-wizard";
import {
  canCommitImport,
  failedRowsCsv,
  isActiveImportStatus,
} from "./import-state";

const ROW_STATUS_OPTIONS: (ImportRowStatus | "all")[] = [
  "all",
  "valid",
  "invalid",
  "duplicate",
  "imported",
  "skipped",
];

const SUMMARY_KEYS = [
  ["total_rows", "totalRows"],
  ["valid_rows", "validRows"],
  ["duplicate_rows", "duplicateRows"],
  ["invalid_rows", "invalidRows"],
  ["imported_rows", "importedRows"],
  ["skipped_rows", "skippedRows"],
  ["error_rows", "errorRows"],
] as const;

/** UI11 가져오기 상세 — 검증 결과·행별 오류·커밋·실패 행 재입력. */
export function ImportDetailPage({ importId }: { importId: string }) {
  const t = useTranslations("imports.detail");
  const tScreens = useTranslations("screens");
  const [commitOpen, setCommitOpen] = useState(false);
  const [wizardOpen, setWizardOpen] = useState(false);

  const query = useQuery({
    queryKey: ["import", importId],
    queryFn: () => getImport(importId),
    // 업로드·검증·커밋 진행 중이면 주기적으로 다시 읽는다.
    refetchInterval: (q) => {
      const job = q.state.data?.data;
      return job && isActiveImportStatus(job.status) ? 3_000 : false;
    },
  });

  if (query.isPending) {
    return (
      <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-6 py-6">
        <Skeleton className="h-8 w-64" />
        <Skeleton className="h-32 w-full rounded-2xl" />
        <Skeleton className="h-64 w-full rounded-2xl" />
      </div>
    );
  }

  if (query.isError || !query.data) {
    const err = query.error;
    const notFound = err instanceof ApiError && err.status === 404;
    return (
      <div className="mx-auto w-full max-w-6xl px-6 py-6">
        {notFound ? (
          <EmptyState title={t("notFound")} />
        ) : (
          <ErrorState
            title={t("loadFailed")}
            error={err}
            onRetry={() => query.refetch()}
          />
        )}
      </div>
    );
  }

  const job = query.data.data;

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-6 py-6">
      <PageHeader
        title={tScreens("importDetail.title")}
        description={tScreens("importDetail.description")}
        actions={
          <div className="flex items-center gap-2">
            <StatusBadge status={job.status} />
            {canCommitImport(job) ? (
              <Button size="sm" onClick={() => setCommitOpen(true)}>
                {t("commit")}
              </Button>
            ) : null}
          </div>
        }
      />

      {isActiveImportStatus(job.status) ? (
        <p role="status" className="text-sm text-muted-foreground">
          {t("pollingNote")}
        </p>
      ) : null}

      <ImportSummaryCard job={job} />
      <ImportMappingCard job={job} />
      <ImportRowsSection importId={importId} />
      <FailedRowsSection
        importId={importId}
        onReenter={() => setWizardOpen(true)}
      />

      <p className="text-2xs text-muted-foreground">{t("basisNote")}</p>

      <CommitImportDialog
        job={job}
        open={commitOpen}
        onOpenChange={setCommitOpen}
      />
      <ImportWizard open={wizardOpen} onOpenChange={setWizardOpen} />
    </div>
  );
}

function ImportSummaryCard({ job }: { job: ImportJob }) {
  const t = useTranslations("imports.detail");
  const summary = job.summary ?? {};
  const items = SUMMARY_KEYS.filter(
    ([key]) => typeof summary[key] === "number"
  );
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("summaryTitle")}</CardTitle>
      </CardHeader>
      <CardContent>
        {items.length === 0 ? (
          <p className="text-sm text-muted-foreground">—</p>
        ) : (
          <dl className="grid grid-cols-2 gap-3 sm:grid-cols-4">
            {items.map(([key, labelKey]) => (
              <div key={key} className="flex flex-col gap-0.5">
                <dt className="text-2xs text-muted-foreground">
                  {t(labelKey)}
                </dt>
                <dd className="text-lg font-semibold">
                  {(summary[key] as number).toLocaleString()}
                </dd>
              </div>
            ))}
          </dl>
        )}
        {job.validation_hash ? (
          <p className="mt-3 break-all text-2xs text-muted-foreground">
            {t("validationHash")}:{" "}
            <span className="font-mono">{job.validation_hash}</span>
          </p>
        ) : null}
      </CardContent>
    </Card>
  );
}

function ImportMappingCard({ job }: { job: ImportJob }) {
  const t = useTranslations("imports.detail");
  const tFields = useTranslations("imports.wizard.fields");
  const entries = Object.entries(job.mapping?.columns ?? {});
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("mappingTitle")}</CardTitle>
      </CardHeader>
      <CardContent>
        {entries.length === 0 ? (
          <p className="text-sm text-muted-foreground">{t("mappingEmpty")}</p>
        ) : (
          <dl className="grid grid-cols-1 gap-x-6 gap-y-1.5 text-sm sm:grid-cols-2">
            {entries.map(([csvColumn, target]) => (
              <div
                key={csvColumn}
                className="grid grid-cols-[1fr_auto_1fr] items-baseline gap-2"
              >
                <dt className="truncate font-mono text-2xs text-muted-foreground">
                  {csvColumn}
                </dt>
                <span aria-hidden className="text-muted-foreground">
                  →
                </span>
                <dd className="min-w-0 truncate">
                  {tFields.has(target) ? tFields(target) : target}
                </dd>
              </div>
            ))}
          </dl>
        )}
      </CardContent>
    </Card>
  );
}

function ImportRowsSection({ importId }: { importId: string }) {
  const t = useTranslations("imports.detail");
  const tStatus = useTranslations("status");
  const [rowStatus, setRowStatus] = useState<ImportRowStatus | "all">("all");
  const query = useImportRows(importId, { status: rowStatus });
  const rows = query.data?.pages.flatMap((p) => p.data) ?? [];
  const last = query.data?.pages.at(-1);

  const columns: ColumnDef<ImportRow>[] = [
    {
      key: "rowNo",
      header: t("colRowNo"),
      cell: (row) => <span className="font-mono text-ui">{row.row_no}</span>,
    },
    {
      key: "status",
      header: t("colStatus"),
      cell: (row) => <StatusBadge status={row.status} />,
    },
    {
      key: "data",
      header: t("colData"),
      cell: (row) => (
        <dl className="grid gap-0.5 text-2xs">
          {Object.entries(row.normalized_data ?? {})
            .slice(0, 4)
            .map(([key, value]) => (
              <div key={key} className="flex gap-1.5">
                <dt className="shrink-0 font-mono text-muted-foreground">
                  {key}:
                </dt>
                <dd className="min-w-0 truncate">
                  {value === null || value === undefined
                    ? "—"
                    : String(value)}
                </dd>
              </div>
            ))}
        </dl>
      ),
    },
    {
      key: "errors",
      header: t("colErrors"),
      cell: (row) =>
        row.errors.length === 0 ? (
          <span className="text-muted-foreground">—</span>
        ) : (
          <ul className="grid gap-0.5 text-2xs text-rose-on-subtle">
            {row.errors.map((err, i) => (
              <li key={i}>
                {[err.field, err.code, err.detail ?? err.message]
                  .filter(Boolean)
                  .join(": ")}
              </li>
            ))}
          </ul>
        ),
    },
  ];

  return (
    <section className="flex flex-col gap-3">
      <div className="flex flex-wrap items-center gap-3">
        <h2 className="text-sm font-semibold">{t("rowsTitle")}</h2>
        <Select
          value={rowStatus}
          onValueChange={(v) => setRowStatus(v as ImportRowStatus | "all")}
        >
          <SelectTrigger
            size="sm"
            aria-label={t("rowFilter")}
            className="w-40"
          >
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {ROW_STATUS_OPTIONS.map((s) => (
              <SelectItem key={s} value={s}>
                {s === "all" ? t("allRows") : tStatus(s)}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>
      <DataTable
        columns={columns}
        rows={rows}
        rowKey={(row) => String(row.row_no)}
        page={last?.page}
        onLoadMore={() => query.fetchNextPage()}
        loadingMore={query.isFetchingNextPage}
        loading={query.isPending}
        error={query.error}
        onRetry={() => query.refetch()}
        emptyState={<EmptyState title={t("rowEmpty")} compact />}
      />
    </section>
  );
}

/**
 * 실패·건너뜀 행 재입력 — 미리보기는 페이지 단위, 다운로드는 모든 행을 조회한다.
 */
function FailedRowsSection({
  importId,
  onReenter,
}: {
  importId: string;
  onReenter: () => void;
}) {
  const t = useTranslations("imports.detail");
  const invalid = useImportRows(importId, { status: "invalid" });
  const skipped = useImportRows(importId, { status: "skipped" });
  const [downloading, setDownloading] = useState(false);
  const [downloadError, setDownloadError] = useState<string | null>(null);

  const failedRows = [
    ...(invalid.data?.pages.flatMap((p) => p.data) ?? []),
    ...(skipped.data?.pages.flatMap((p) => p.data) ?? []),
  ];
  const loading = invalid.isPending || skipped.isPending;
  async function downloadCsv() {
    setDownloading(true);
    setDownloadError(null);
    try {
      const allRows: ImportRow[] = [];
      for (const status of ["invalid", "skipped"] as const) {
        let afterRowNo = 0;
        while (true) {
          const page = await getImportRowsPage(importId, { status, afterRowNo, limit: 200 });
          allRows.push(...page.data);
          if (!page.page?.has_more || !page.page.next_cursor) break;
          const next = Number(page.page.next_cursor);
          if (!Number.isInteger(next) || next <= afterRowNo) throw new Error(t("downloadFailed"));
          afterRowNo = next;
        }
      }
      if (allRows.length === 0) return;
      // BOM lets spreadsheet apps open UTF-8 Korean text without corruption.
      const blob = new Blob(["\uFEFF" + failedRowsCsv(allRows)], {
        type: "text/csv;charset=utf-8",
      });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = `import-${importId}-failed-rows.csv`;
      a.click();
      URL.revokeObjectURL(url);
    } catch (cause) {
      setDownloadError(cause instanceof Error ? cause.message : t("downloadFailed"));
    } finally {
      setDownloading(false);
    }
  }

  return (
    <section className="flex flex-col gap-2 rounded-2xl border border-border px-4 py-3">
      <h2 className="text-sm font-semibold">{t("failedRowsTitle")}</h2>
      <p className="text-sm text-muted-foreground">
        {t("failedRowsDescription")}
      </p>
      {loading ? (
        <Skeleton className="h-8 w-full" />
      ) : failedRows.length === 0 ? (
        <p className="text-sm text-muted-foreground">{t("failedRowsEmpty")}</p>
      ) : (
        <>
          <p className="text-2xs text-muted-foreground">
            {t("failedRowsLoadedNote", {
              loaded: failedRows.length,
            })}
          </p>
          {downloadError ? <p role="alert" className="text-sm text-destructive">{downloadError}</p> : null}
          <div className="flex flex-wrap gap-2">
            <Button size="sm" variant="outline" disabled={downloading} onClick={() => void downloadCsv()}>
              {downloading ? t("downloadingCsv") : t("downloadCsv")}
            </Button>
            <Button size="sm" variant="outline" onClick={onReenter}>
              {t("reenter")}
            </Button>
          </div>
        </>
      )}
    </section>
  );
}

/**
 * 커밋 확인 — 되돌릴 수 없는 작업이므로 확인 문구 입력을 요구한다
 * (ConfirmIrreversible 의 발송 필드가 맞지 않아 같은 규칙을 영역 대화상자로
 * 구현한다). validation_hash + If-Match(version) + 멱등 키를 보낸다.
 */
function CommitImportDialog({
  job,
  open,
  onOpenChange,
}: {
  job: ImportJob;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const t = useTranslations("imports.detail");
  const tCommon = useTranslations("common");
  const queryClient = useQueryClient();
  const [policy, setPolicy] = useState<"skip" | "reject">("skip");
  const [typed, setTyped] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const idemRef = useRef<string>(idempotencyKey());

  // 닫힐 때 확인 입력·오류를 지워 다음 열림이 초기 상태가 되게 한다.
  function handleOpenChange(next: boolean) {
    if (!next) {
      setTyped("");
      setError(null);
    }
    onOpenChange(next);
  }

  const confirmText = t("confirmText");
  const ready = canCommitImport(job);
  const confirmed = typed === confirmText;

  async function onConfirm() {
    if (!job.validation_hash) return;
    setPending(true);
    setError(null);
    try {
      const jobId = importJobId(job);
      await commitImport(
        jobId,
        {
          validation_hash: job.validation_hash,
          invalid_row_policy: policy,
        },
        { idempotencyKey: idemRef.current }
      );
      idemRef.current = idempotencyKey();
      toast.success(t("commitRequested"));
      await queryClient.invalidateQueries({ queryKey: ["import", jobId] });
      await queryClient.invalidateQueries({ queryKey: ["import-rows", jobId] });
      await queryClient.invalidateQueries({ queryKey: ["imports"] });
      handleOpenChange(false);
    } catch (err) {
      const apiErr = err instanceof ApiError ? err : null;
      if (apiErr?.status === 409) {
        setError(
          apiErr.code === "SNAPSHOT_MISMATCH"
            ? t("snapshotMismatch")
            : t("versionConflict")
        );
      } else if (apiErr?.status === 403) {
        setError(t("forbidden"));
      } else {
        setError(apiErr?.problem.detail ?? t("commitFailed"));
      }
      void queryClient.invalidateQueries({
        queryKey: ["import", importJobId(job)],
      });
      if (apiErr && apiErr.problem.retryable !== true && apiErr.status < 500) {
        idemRef.current = idempotencyKey();
      }
    } finally {
      setPending(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("commitTitle")}</DialogTitle>
          <DialogDescription>{t("commitDescription")}</DialogDescription>
        </DialogHeader>
        {!ready ? (
          <p role="alert" className="text-sm text-muted-foreground">
            {t("commitOnlyReady")}
          </p>
        ) : (
          <div className="grid gap-4">
            <div className="grid gap-1.5">
              <Label htmlFor="invalid-policy">{t("invalidPolicy")}</Label>
              <Select
                value={policy}
                onValueChange={(v) => setPolicy(v as "skip" | "reject")}
              >
                <SelectTrigger id="invalid-policy" className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="skip">{t("invalidSkip")}</SelectItem>
                  <SelectItem value="reject">{t("invalidReject")}</SelectItem>
                </SelectContent>
              </Select>
              <p className="text-2xs text-muted-foreground">
                {t("partialNote")}
              </p>
            </div>
            <div className="grid gap-1.5">
              <Label htmlFor="commit-confirm-input" className="text-sm">
                {tCommon("confirmIrreversible.typeToConfirm", {
                  text: confirmText,
                })}
              </Label>
              <Input
                id="commit-confirm-input"
                value={typed}
                onChange={(e) => setTyped(e.target.value)}
                placeholder={confirmText}
                autoComplete="off"
                aria-invalid={typed.length > 0 && !confirmed}
              />
            </div>
          </div>
        )}
        {error ? (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        ) : null}
        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            onClick={() => handleOpenChange(false)}
          >
            {tCommon("cancel")}
          </Button>
          <Button
            type="button"
            variant="destructive"
            disabled={!ready || !confirmed || pending}
            onClick={() => void onConfirm()}
          >
            {pending ? t("committing") : t("commit")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
