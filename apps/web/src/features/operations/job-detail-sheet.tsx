"use client";

/**
 * 작업 상세 시트 — 상태·리스·오류·관련 리소스와 운영 행동.
 * unknown 결과에는 "재시도" 대신 조사(대조) 안내를 보인다 (UI13, §16.3).
 */
import { OctagonXIcon, TriangleAlertIcon } from "lucide-react";
import { useLocale, useTranslations } from "next-intl";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useRef, useState, type FormEvent } from "react";
import { toast } from "sonner";

import { ErrorState } from "@/components/patterns/error-state";
import { StatusBadge } from "@/components/patterns/status-badge";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import { Skeleton } from "@/components/ui/skeleton";
import { idempotencyKey } from "@/lib/api/client";
import { ApiError } from "@/lib/api/problem";
import {
  getJob,
  requestJobRetry,
  type JobDetail,
} from "@/lib/api/operations";
import { usePrincipals } from "@/lib/api/settings";
import { jobPrimaryAction } from "@/features/operations/job-rules";
import { apiErrorText } from "@/features/settings/api-error";
import { formatDateTime } from "@/features/settings/format";

interface JobDetailSheetProps {
  jobId: string | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="grid grid-cols-[8rem_1fr] gap-2 py-1 text-sm">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="min-w-0 break-words">{children}</dd>
    </div>
  );
}

export function JobDetailSheet({ jobId, open, onOpenChange }: JobDetailSheetProps) {
  const t = useTranslations("operations.jobs");
  const tOps = useTranslations("operations");
  const tc = useTranslations("common");
  const locale = useLocale();
  const principals = usePrincipals();
  const queryClient = useQueryClient();
  const [retryReason, setRetryReason] = useState("");
  const [actionError, setActionError] = useState<string | null>(null);
  const idemRef = useRef(idempotencyKey());

  const detail = useQuery({
    queryKey: ["operations", "jobs", "detail", jobId],
    queryFn: () => getJob(jobId!),
    enabled: open && !!jobId,
  });

  const retry = useMutation({
    mutationFn: () =>
      requestJobRetry(
        jobId!,
        { reason: retryReason.trim() || undefined },
        { idempotencyKey: idemRef.current }
      ),
    onSuccess: () => {
      toast.success(t("retry.success"));
      setActionError(null);
      idemRef.current = idempotencyKey();
      queryClient.invalidateQueries({ queryKey: ["operations", "jobs"] });
      detail.refetch();
    },
    onError: (err) => {
      setActionError(apiErrorText(tOps, err));
      if (err instanceof ApiError && err.problem.retryable !== true && err.status < 500) {
        idemRef.current = idempotencyKey();
      }
    },
  });

  const job: JobDetail | undefined = detail.data?.data;
  const action = job ? jobPrimaryAction(job) : "none";
  const requester = job?.requested_by
    ? principals.data?.pages
        .flatMap((page) => page.data)
        .find((principal) => principal.id === job.requested_by)
    : undefined;

  function submitRetry(e: FormEvent<HTMLFormElement>) {
    e.preventDefault();
    if (retry.isPending) return;
    retry.mutate();
  }

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent side="right" className="w-full sm:max-w-lg overflow-y-auto">
        <SheetHeader>
          <SheetTitle>{t("detail.title")}</SheetTitle>
          <SheetDescription>{t("detail.description")}</SheetDescription>
        </SheetHeader>
        <div className="flex flex-col gap-4 px-4 pb-6">
          {detail.isPending ? (
            <div className="flex flex-col gap-2" aria-busy="true">
              {Array.from({ length: 6 }).map((_, i) => (
                <Skeleton key={i} className="h-5 w-full" />
              ))}
            </div>
          ) : detail.isError || !job ? (
            <ErrorState error={detail.error} onRetry={() => detail.refetch()} />
          ) : (
            <>
              <dl className="divide-y divide-border/60">
                <Row label={t("detail.kind")}>
                  <span className="font-mono text-xs">{job.kind}</span>
                </Row>
                <Row label={t("detail.state")}>
                  <StatusBadge status={job.state} />
                </Row>
                <Row label={t("detail.attempts")}>{job.attempt_count}</Row>
                <Row label={t("columns.lastError")}>{job.last_error_code ?? "—"}</Row>
                <Row label={t("detail.availableAt")}>{formatDateTime(job.available_at, locale)}</Row>
                <Row label={t("detail.createdAt")}>{formatDateTime(job.created_at, locale)}</Row>
                <Row label={t("detail.finishedAt")}>{formatDateTime(job.finished_at, locale)}</Row>
                <Row label={t("detail.leaseOwner")}>{job.lease_owner ?? "—"}</Row>
                <Row label={t("detail.leaseExpires")}>{formatDateTime(job.lease_expires_at, locale)}</Row>
                <Row label={t("detail.requestedBy")}>
                  {job.requested_by
                    ? requester?.name?.trim() || requester?.display_name?.trim() || tc("nameUnavailable")
                    : "—"}
                </Row>
                <Row label={t("detail.payloadVersion")}>{job.payload_version}</Row>
                {job.external_effect ? (
                  <Row label={t("detail.externalEffect")}>
                    {t("detail.externalEffectYes")}
                  </Row>
                ) : null}
                {job.error_detail ? (
                  <Row label={t("detail.errorDetail")}>
                    <span className="whitespace-pre-wrap">{job.error_detail}</span>
                  </Row>
                ) : null}
                {job.result_refs && job.result_refs.length > 0 ? (
                  <Row label={t("detail.resultRefs")}>
                    <ul className="flex flex-col gap-0.5">
                      {job.result_refs.map((ref) => (
                        <li key={`${ref.type}:${ref.id}`} className="font-mono text-xs">
                          {ref.type}: {ref.id}
                        </li>
                      ))}
                    </ul>
                  </Row>
                ) : null}
              </dl>

              {action === "investigate" ? (
                <Alert variant="warning">
                  <TriangleAlertIcon />
                  <AlertTitle>{t("investigate.title")}</AlertTitle>
                  <AlertDescription>
                    <p>{t("investigate.description")}</p>
                    <p className="text-2xs">{t("investigate.noResend")}</p>
                  </AlertDescription>
                </Alert>
              ) : null}

              {action === "retry" ? (
                <form onSubmit={submitRetry} className="flex flex-col gap-2 rounded-2xl border border-border p-3">
                  <p className="text-2xs text-muted-foreground">{t("retry.confirmHint")}</p>
                  <div className="grid gap-1.5">
                    <Label htmlFor="job-retry-reason">{t("retry.reason")}</Label>
                    <Input
                      id="job-retry-reason"
                      value={retryReason}
                      onChange={(e) => setRetryReason(e.target.value)}
                      placeholder={t("retry.reasonPlaceholder")}
                    />
                  </div>
                  <div>
                    <Button type="submit" size="sm" disabled={retry.isPending}>
                      {t("retry.submit")}
                    </Button>
                  </div>
                </form>
              ) : null}

              {job.retry_block_reason ? (
                <p className="flex items-center gap-1.5 text-2xs text-muted-foreground">
                  <OctagonXIcon className="size-3" aria-hidden />
                  {job.retry_block_reason}
                </p>
              ) : null}

              {actionError ? (
                <p role="alert" className="text-sm text-destructive">
                  {actionError}
                </p>
              ) : null}
            </>
          )}
        </div>
      </SheetContent>
    </Sheet>
  );
}
