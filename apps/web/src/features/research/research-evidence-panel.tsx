"use client";

import { useQueryClient } from "@tanstack/react-query";
import { useTranslations } from "next-intl";
import { useRef, useState } from "react";
import { toast } from "sonner";

import { EmptyState } from "@/components/patterns/empty-state";
import { ErrorState } from "@/components/patterns/error-state";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { Textarea } from "@/components/ui/textarea";
import { idempotencyKey } from "@/lib/api/client";
import { ApiError } from "@/lib/api/problem";
import {
  reviewResearchEvidence,
  useCompanyResearchEvidence,
  type EvidenceReviewVerdict,
  type ResearchEvidence,
} from "@/lib/api/research";

import { EvidenceList } from "./evidence-list";

/**
 * 기업 상세 Research 탭용 증빙 패널 (§7.4 — Task 14 가 마운트한다).
 * AI 가 보완한 필드의 출처·관측 시각·검토 상태를 보여주고 사람의
 * verified/rejected 판단을 기록한다. 이 판단은 발송 근거를 만들지 않는다.
 */
export function ResearchEvidencePanel({ companyId }: { companyId: string }) {
  const t = useTranslations("research.panel");
  const tEvidence = useTranslations("research.evidence");
  const tTable = useTranslations("common.table");
  const [pendingOnly, setPendingOnly] = useState(true);
  const [reviewTarget, setReviewTarget] = useState<{
    evidence: ResearchEvidence;
    verdict: EvidenceReviewVerdict;
  } | null>(null);

  const query = useCompanyResearchEvidence({
    companyId,
    reviewStatus: pendingOnly ? "pending" : "all",
  });
  const items = query.data?.pages.flatMap((p) => p.data) ?? [];
  const last = query.data?.pages.at(-1);

  return (
    <section className="flex flex-col gap-3" aria-labelledby="research-panel-title">
      <div className="flex flex-wrap items-start gap-3">
        <div className="min-w-0 flex-1">
          <h2 id="research-panel-title" className="text-sm font-semibold">
            {t("title")}
          </h2>
          <p className="mt-1 text-sm text-muted-foreground">
            {t("description")}
          </p>
        </div>
        <Select
          value={pendingOnly ? "pending" : "all"}
          onValueChange={(v) => setPendingOnly(v === "pending")}
        >
          <SelectTrigger size="sm" aria-label={t("pendingOnly")} className="w-36">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="pending">{t("pendingOnly")}</SelectItem>
            <SelectItem value="all">{t("all")}</SelectItem>
          </SelectContent>
        </Select>
      </div>

      {query.isPending ? (
        <div className="flex flex-col gap-2">
          <Skeleton className="h-24 w-full rounded-2xl" />
          <Skeleton className="h-24 w-full rounded-2xl" />
        </div>
      ) : query.isError ? (
        <ErrorState
          title={tEvidence("loadFailed")}
          error={query.error}
          onRetry={() => query.refetch()}
        />
      ) : items.length === 0 ? (
        <EmptyState title={tEvidence("empty")} compact />
      ) : (
        <EvidenceList
          evidence={items}
          actions={(item) =>
            item.review_status === "pending" ? (
              <>
                <Button
                  size="xs"
                  variant="outline"
                  onClick={() =>
                    setReviewTarget({ evidence: item, verdict: "verified" })
                  }
                >
                  {tEvidence("verify")}
                </Button>
                <Button
                  size="xs"
                  variant="outline"
                  onClick={() =>
                    setReviewTarget({ evidence: item, verdict: "rejected" })
                  }
                >
                  {tEvidence("reject")}
                </Button>
              </>
            ) : null
          }
        />
      )}

      {last?.page?.has_more ? (
        <div className="flex justify-center">
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={query.isFetchingNextPage}
            onClick={() => query.fetchNextPage()}
          >
            {query.isFetchingNextPage
              ? tTable("loading")
              : tTable("loadMore")}
          </Button>
        </div>
      ) : null}

      <p className="text-2xs text-muted-foreground">
        {tEvidence("confidenceNote")}
      </p>

      {reviewTarget ? (
        <EvidenceReviewDialog
          companyId={companyId}
          evidence={reviewTarget.evidence}
          verdict={reviewTarget.verdict}
          open
          onOpenChange={(open) => {
            if (!open) setReviewTarget(null);
          }}
        />
      ) : null}
    </section>
  );
}

/**
 * 증빙 검토 대화상자 — research_evidence 에는 version 필드가 없어
 * If-Match 없이 보낸다 (멱등 키만). 검토는 발송 근거를 자동 생성하지 않는다.
 */
function EvidenceReviewDialog({
  companyId,
  evidence,
  verdict,
  open,
  onOpenChange,
}: {
  companyId: string;
  evidence: ResearchEvidence;
  verdict: EvidenceReviewVerdict;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const t = useTranslations("research.evidence");
  const tCommon = useTranslations("common");
  const queryClient = useQueryClient();
  const [reason, setReason] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const idemRef = useRef<string>(idempotencyKey());

  async function onConfirm() {
    setPending(true);
    setError(null);
    try {
      await reviewResearchEvidence(
        evidence.id,
        { review_status: verdict, reason: reason.trim() || undefined },
        { idempotencyKey: idemRef.current }
      );
      idemRef.current = idempotencyKey();
      toast.success(t("success"));
      await queryClient.invalidateQueries({
        queryKey: ["research-evidence", companyId],
      });
      onOpenChange(false);
    } catch (err) {
      const apiErr = err instanceof ApiError ? err : null;
      setError(
        apiErr?.status === 403
          ? t("forbidden")
          : (apiErr?.problem.detail ?? t("failed"))
      );
      if (apiErr && apiErr.problem.retryable !== true && apiErr.status < 500) {
        idemRef.current = idempotencyKey();
      }
    } finally {
      setPending(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>
            {verdict === "verified"
              ? t("reviewTitleVerify")
              : t("reviewTitleReject")}
          </DialogTitle>
          <DialogDescription>{t("reviewDescription")}</DialogDescription>
        </DialogHeader>
        <div className="grid gap-4">
          <dl className="grid grid-cols-[7rem_1fr] gap-x-3 gap-y-1.5 rounded-2xl border border-border bg-app-panel-muted px-4 py-3 text-sm">
            <dt className="text-muted-foreground">{t("fieldPath")}</dt>
            <dd className="font-mono text-ui">{evidence.field_path}</dd>
            <dt className="text-muted-foreground">{t("value")}</dt>
            <dd className="break-words">
              {typeof evidence.value === "string"
                ? evidence.value
                : JSON.stringify(evidence.value)}
            </dd>
            <dt className="text-muted-foreground">{t("sourceUrl")}</dt>
            <dd className="min-w-0 break-all font-mono text-2xs">
              {evidence.source_url}
            </dd>
          </dl>
          <div className="grid gap-1.5">
            <Label htmlFor="evidence-review-reason">{t("reason")}</Label>
            <Textarea
              id="evidence-review-reason"
              rows={2}
              value={reason}
              onChange={(e) => setReason(e.target.value)}
            />
          </div>
          {error ? (
            <p role="alert" className="text-sm text-destructive">
              {error}
            </p>
          ) : null}
        </div>
        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            onClick={() => onOpenChange(false)}
          >
            {tCommon("cancel")}
          </Button>
          <Button
            type="button"
            variant={verdict === "rejected" ? "destructive" : "default"}
            disabled={pending}
            onClick={() => void onConfirm()}
          >
            {pending ? t("submitting") : t("submit")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
