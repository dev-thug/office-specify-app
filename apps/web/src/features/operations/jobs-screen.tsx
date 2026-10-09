"use client";

/**
 * UI13 운영 화면 — 작업 큐 + 감사 로그 + 긴급 중지.
 * 상단: 긴급 중지 배너(활성 시)와 운영 알림(unknown·dead·재인증).
 * unknown 결과는 재시도 버튼 없이 조사 안내만 한다.
 */
import { OctagonXIcon, TriangleAlertIcon } from "lucide-react";
import { useLocale, useTranslations } from "next-intl";
import { useMemo, useState } from "react";

import { DataTable, type ColumnDef } from "@/components/patterns/data-table";
import { EmptyState } from "@/components/patterns/empty-state";
import { PageHeader } from "@/components/patterns/page-header";
import { StatusBadge } from "@/components/patterns/status-badge";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
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
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { useJobs, type Job, type JobState } from "@/lib/api/operations";
import { useMailAccounts } from "@/lib/api/settings";
import { AuditLogTable } from "@/features/operations/audit-log-table";
import { EmergencyStopCard } from "@/features/operations/emergency-stop";
import { JobDetailSheet } from "@/features/operations/job-detail-sheet";
import { jobPrimaryAction } from "@/features/operations/job-rules";
import { formatDateTime } from "@/features/settings/format";

const STATE_OPTIONS: JobState[] = [
  "queued",
  "running",
  "retry_wait",
  "completed",
  "dead",
  "canceled",
  "unknown",
];

export function JobsScreen() {
  const t = useTranslations("operations.jobs");
  const tOps = useTranslations("operations");
  const locale = useLocale();
  const [stateFilter, setStateFilter] = useState<JobState | "">("");
  const [kindFilter, setKindFilter] = useState("");
  const [detailId, setDetailId] = useState<string | null>(null);

  const jobs = useJobs({ state: stateFilter, kind: kindFilter });
  const rows = jobs.data?.pages.flatMap((p) => p.data) ?? [];
  const page = jobs.data?.pages.at(-1)?.page;

  // 알림 전용 카운트 — 필터와 무관하게 unknown·dead·재인증 존재 여부를 잡는다.
  const unknownJobs = useJobs({ state: "unknown" });
  const deadJobs = useJobs({ state: "dead" });
  const mailAccounts = useMailAccounts();
  const unknownCount = useMemo(
    () => unknownJobs.data?.pages.flatMap((p) => p.data).length ?? 0,
    [unknownJobs.data]
  );
  const deadCount = useMemo(
    () => deadJobs.data?.pages.flatMap((p) => p.data).length ?? 0,
    [deadJobs.data]
  );
  const reauthCount = useMemo(
    () =>
      mailAccounts.data?.pages
        .flatMap((p) => p.data)
        .filter((a) => a.status === "reauth_required").length ?? 0,
    [mailAccounts.data]
  );

  const columns: ColumnDef<Job>[] = [
    {
      key: "kind",
      header: t("columns.kind"),
      cell: (row) => <span className="font-mono text-xs">{row.kind}</span>,
    },
    {
      key: "state",
      header: t("columns.state"),
      cell: (row) => <StatusBadge status={row.state} />,
    },
    {
      key: "attempts",
      header: t("columns.attempts"),
      cell: (row) => <span className="tabular-nums">{row.attempt_count}</span>,
    },
    {
      key: "lastError",
      header: t("columns.lastError"),
      cell: (row) => (
        <span className="font-mono text-xs">{row.last_error_code ?? "—"}</span>
      ),
    },
    {
      key: "availableAt",
      header: t("columns.availableAt"),
      cell: (row) => (
        <span className="whitespace-nowrap text-xs">
          {formatDateTime(row.available_at, locale)}
        </span>
      ),
    },
    {
      key: "createdAt",
      header: t("columns.createdAt"),
      cell: (row) => (
        <span className="whitespace-nowrap text-xs">
          {formatDateTime(row.created_at, locale)}
        </span>
      ),
    },
    {
      key: "actions",
      header: t("columns.actions"),
      cell: (row) => {
        const action = jobPrimaryAction(row);
        return (
          <div className="flex items-center gap-1.5">
            {action === "investigate" ? (
              <Badge variant="amber-subtle">
                <TriangleAlertIcon aria-hidden />
                {t("investigate.badge")}
              </Badge>
            ) : null}
            <Button
              type="button"
              variant="ghost"
              size="xs"
              onClick={() => setDetailId(row.id)}
            >
              {t("openDetail")}
            </Button>
          </div>
        );
      },
    },
  ];

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-6 py-6">
      <PageHeader title={t("title")} description={t("description")} />

      <EmergencyStopCard />

      {unknownCount > 0 ? (
        <Alert variant="warning">
          <TriangleAlertIcon />
          <AlertTitle>{tOps("alerts.unknownJobs", { count: unknownCount })}</AlertTitle>
          <AlertDescription>
            <Button
              type="button"
              variant="link"
              size="xs"
              onClick={() => setStateFilter("unknown")}
            >
              {t("openDetail")}
            </Button>
          </AlertDescription>
        </Alert>
      ) : null}
      {deadCount > 0 ? (
        <Alert variant="warning">
          <OctagonXIcon />
          <AlertTitle>{tOps("alerts.deadJobs", { count: deadCount })}</AlertTitle>
          <AlertDescription>
            <Button
              type="button"
              variant="link"
              size="xs"
              onClick={() => setStateFilter("dead")}
            >
              {t("openDetail")}
            </Button>
          </AlertDescription>
        </Alert>
      ) : null}
      {reauthCount > 0 ? (
        <Alert variant="warning">
          <TriangleAlertIcon />
          <AlertTitle>{tOps("alerts.reauth", { count: reauthCount })}</AlertTitle>
        </Alert>
      ) : null}

      <Tabs defaultValue="jobs">
        <TabsList>
          <TabsTrigger value="jobs">{t("tabs.jobs")}</TabsTrigger>
          <TabsTrigger value="audit">{t("tabs.audit")}</TabsTrigger>
        </TabsList>
        <TabsContent value="jobs" className="flex flex-col gap-3">
          <div className="flex flex-wrap items-center gap-2">
            <Select
              value={stateFilter || "__all__"}
              onValueChange={(v) => setStateFilter(v === "__all__" ? "" : (v as JobState))}
            >
              <SelectTrigger size="sm" aria-label={t("filterState")}>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="__all__">{t("filterStateAll")}</SelectItem>
                {STATE_OPTIONS.map((s) => (
                  <SelectItem key={s} value={s}>
                    {s}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <Input
              value={kindFilter}
              onChange={(e) => setKindFilter(e.target.value)}
              placeholder={t("filterKindPlaceholder")}
              aria-label={t("filterKind")}
              className="w-56"
            />
          </div>
          <DataTable
            columns={columns}
            rows={rows}
            rowKey={(row) => row.id}
            loading={jobs.isPending}
            error={jobs.error}
            onRetry={() => jobs.refetch()}
            page={page}
            onLoadMore={() => jobs.fetchNextPage()}
            loadingMore={jobs.isFetchingNextPage}
            emptyState={
              <EmptyState title={t("emptyTitle")} description={t("emptyDescription")} />
            }
            caption={t("title")}
          />
        </TabsContent>
        <TabsContent value="audit">
          <AuditLogTable />
        </TabsContent>
      </Tabs>

      <JobDetailSheet
        jobId={detailId}
        open={detailId !== null}
        onOpenChange={(open) => !open && setDetailId(null)}
      />
    </div>
  );
}
