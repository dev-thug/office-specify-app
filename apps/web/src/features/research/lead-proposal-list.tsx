"use client";

import { useQueryClient } from "@tanstack/react-query";
import { useFormatter, useTranslations } from "next-intl";
import Link from "next/link";
import { useRef, useState } from "react";
import { toast } from "sonner";

import { EmptyState } from "@/components/patterns/empty-state";
import { ErrorState } from "@/components/patterns/error-state";
import { StatusBadge } from "@/components/patterns/status-badge";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardFooter,
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
  decideLeadProposal,
  useLeadProposals,
  type LeadDecision,
  type LeadProposal,
  type LeadProposalStatus,
} from "@/lib/api/research";

import { EvidenceList } from "./evidence-list";
import {
  canDecideProposal,
  displayValue,
  findUnverifiedEmails,
} from "./proposal-utils";

const STATUS_OPTIONS: (LeadProposalStatus | "all")[] = [
  "all",
  "pending",
  "needs_review",
  "accepted",
  "rejected",
];

/**
 * 서버가 allowed_actions 를 보낼 때 결정 버튼을 여는 데 요구하는 action 이름.
 * B.2는 문자열 목록만 정의한다 — "decide" 로 가정 (report 의 assumptions 참조).
 */
const DECIDE_ACTION = "decide";

/**
 * 리드 후보 검토 목록 — 상태 필터 + 카드형 목록 + 채택/거절 결정 (UI10, §13.4).
 * `agentRunId` 를 주면 해당 실행이 제출한 후보만 보여준다.
 */
export function LeadProposalReviewList({
  agentRunId,
}: {
  agentRunId?: string;
}) {
  const t = useTranslations("research.proposals");
  const tStatus = useTranslations("status");
  const tTable = useTranslations("common.table");
  const [status, setStatus] = useState<LeadProposalStatus | "all">(
    agentRunId ? "all" : "pending"
  );
  const [decisionTarget, setDecisionTarget] = useState<{
    proposal: LeadProposal;
    decision: LeadDecision;
  } | null>(null);

  const query = useLeadProposals({ status, agentRunId });
  const allProposals = query.data?.pages.flatMap((p) => p.data) ?? [];
  const proposals = agentRunId
    ? allProposals.filter((proposal) => proposal.agent_run_id === agentRunId)
    : allProposals;
  const last = query.data?.pages.at(-1);

  return (
    <div className="flex flex-col gap-3">
      <div className="flex items-center gap-2">
        <Label htmlFor="proposal-status-filter" className="sr-only">
          {t("statusFilter")}
        </Label>
        <Select
          value={status}
          onValueChange={(v) => setStatus(v as LeadProposalStatus | "all")}
        >
          <SelectTrigger
            id="proposal-status-filter"
            size="sm"
            aria-label={t("statusFilter")}
            className="w-44"
          >
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {STATUS_OPTIONS.map((s) => (
              <SelectItem key={s} value={s}>
                {s === "all" ? t("allStatuses") : tStatus(s)}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>

      {query.isPending ? (
        <div className="flex flex-col gap-3">
          <Skeleton className="h-40 w-full rounded-2xl" />
          <Skeleton className="h-40 w-full rounded-2xl" />
        </div>
      ) : query.isError ? (
        <ErrorState
          title={t("loadFailed")}
          error={query.error}
          onRetry={() => query.refetch()}
        />
      ) : proposals.length === 0 ? (
        <EmptyState
          title={t("empty")}
          description={t("emptyDescription")}
        />
      ) : (
        <ul className="flex flex-col gap-3">
          {proposals.map((proposal) => (
            <li key={proposal.id}>
              <ProposalCard
                proposal={proposal}
                onDecide={(decision) =>
                  setDecisionTarget({ proposal, decision })
                }
              />
            </li>
          ))}
        </ul>
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

      {decisionTarget ? (
        <ProposalDecisionDialog
          proposal={decisionTarget.proposal}
          decision={decisionTarget.decision}
          open
          onOpenChange={(open) => {
            if (!open) setDecisionTarget(null);
          }}
        />
      ) : null}
    </div>
  );
}

function ProposalCard({
  proposal,
  onDecide,
}: {
  proposal: LeadProposal;
  onDecide: (decision: LeadDecision) => void;
}) {
  const t = useTranslations("research.proposals");
  const format = useFormatter();

  const flaggedEmails = findUnverifiedEmails(proposal.proposed_data ?? {});
  const dataEntries = Object.entries(proposal.proposed_data ?? {});
  const duplicates = proposal.duplicate_candidates ?? [];
  const evidence = proposal.evidence ?? [];
  const decidable =
    canDecideProposal(proposal.status) &&
    (!proposal.allowed_actions ||
      proposal.allowed_actions.includes(DECIDE_ACTION));

  return (
    <Card>
      <CardHeader>
        <div className="flex flex-wrap items-center gap-2">
          <CardTitle className="text-base">{proposal.company_name}</CardTitle>
          {proposal.domain ? (
            <span className="text-sm text-muted-foreground">
              {proposal.domain}
            </span>
          ) : null}
          <StatusBadge status={proposal.status} />
          {duplicates.length > 0 ? (
            <Badge variant="amber-subtle">
              {t("duplicates", { count: duplicates.length })}
            </Badge>
          ) : null}
          <span className="ms-auto text-2xs text-muted-foreground">
            {t("createdAt")}:{" "}
            {format.dateTime(new Date(proposal.created_at), {
              dateStyle: "medium",
              timeStyle: "short",
            })}
          </span>
        </div>
      </CardHeader>

      <CardContent className="flex flex-col gap-4">
        {proposal.external_key ? (
          <p className="text-2xs text-muted-foreground">
            {t("externalKey")}:{" "}
            <span className="font-mono">{proposal.external_key}</span>
          </p>
        ) : null}

        {flaggedEmails.length > 0 ? (
          <div className="flex flex-col gap-1.5 rounded-2xl border border-amber/30 bg-amber/5 px-4 py-3">
            <ul className="flex flex-wrap gap-2">
              {flaggedEmails.map((flag) => (
                <li
                  key={flag.path}
                  className="flex flex-wrap items-center gap-1.5"
                >
                  <span className="font-mono text-ui">{flag.value}</span>
                  <Badge variant="amber-subtle">{t("unverifiedEmail")}</Badge>
                  <Badge variant="secondary">{t("pendingBasisBadge")}</Badge>
                </li>
              ))}
            </ul>
            <p className="text-2xs text-muted-foreground">
              {t("unverifiedEmailNote")}
            </p>
          </div>
        ) : null}

        {dataEntries.length > 0 ? (
          <div className="flex flex-col gap-1">
            <p className="text-2xs font-medium text-muted-foreground">
              {t("proposedData")}
            </p>
            <dl className="grid grid-cols-1 gap-x-6 gap-y-1 text-sm sm:grid-cols-2">
              {dataEntries.map(([key, value]) => (
                <div
                  key={key}
                  className="grid grid-cols-[8rem_1fr] items-baseline gap-2"
                >
                  <dt className="truncate font-mono text-2xs text-muted-foreground">
                    {key}
                  </dt>
                  <dd className="min-w-0 break-words">{displayValue(value)}</dd>
                </div>
              ))}
            </dl>
          </div>
        ) : (
          <p className="text-sm text-muted-foreground">{t("noData")}</p>
        )}

        <div className="flex flex-col gap-1.5">
          <p className="text-2xs font-medium text-muted-foreground">
            {t("evidenceCount", { count: evidence.length })}
          </p>
          {evidence.length > 0 ? (
            <EvidenceList evidence={evidence} />
          ) : (
            <p className="text-sm text-muted-foreground">{t("noEvidence")}</p>
          )}
        </div>
      </CardContent>

      <CardFooter className="flex flex-wrap items-center gap-2">
        {decidable ? (
          <>
            <Button size="sm" onClick={() => onDecide("accept")}>
              {t("accept")}
            </Button>
            <Button
              size="sm"
              variant="outline"
              onClick={() => onDecide("reject")}
            >
              {t("reject")}
            </Button>
          </>
        ) : null}
        {proposal.status === "accepted" && proposal.accepted_company_id ? (
          <Button variant="link" size="xs" asChild className="px-0">
            <Link href={`/app/companies/${proposal.accepted_company_id}`}>
              {t("viewCompany")}
            </Link>
          </Button>
        ) : null}
        {proposal.reviewed_by || proposal.reviewed_at ? (
          <span className="ms-auto flex flex-wrap items-baseline gap-x-3 text-2xs text-muted-foreground">
            {proposal.reviewed_by ? (
              <span>
                {t("reviewedBy")}:{" "}
                <span className="font-mono">{proposal.reviewed_by}</span>
              </span>
            ) : null}
            {proposal.reviewed_at ? (
              <span>
                {t("reviewedAt")}:{" "}
                {format.dateTime(new Date(proposal.reviewed_at), {
                  dateStyle: "medium",
                  timeStyle: "short",
                })}
              </span>
            ) : null}
          </span>
        ) : null}
      </CardFooter>
    </Card>
  );
}

const NEW_COMPANY = "__new__";

/**
 * 채택·거절 결정 대화상자 — lead_proposals.version 을 If-Match 로 보낸다
 * (B.2). 채택 시 중복 후보가 있으면 기존 기업 병합을 선택할 수 있다.
 */
function ProposalDecisionDialog({
  proposal,
  decision,
  open,
  onOpenChange,
}: {
  proposal: LeadProposal;
  decision: LeadDecision;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const t = useTranslations("research.proposals.decision");
  const tCommon = useTranslations("common");
  const queryClient = useQueryClient();
  const duplicates = proposal.duplicate_candidates ?? [];
  const [mergeInto, setMergeInto] = useState<string>(NEW_COMPANY);
  const [reason, setReason] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // 논리 작업당 하나의 멱등 키 — 확정 응답 후에만 재발급한다.
  const idemRef = useRef<string>(idempotencyKey());

  async function onConfirm() {
    setPending(true);
    setError(null);
    try {
      await decideLeadProposal(
        proposal.id,
        {
          decision,
          merge_into_company_id:
            decision === "accept" && mergeInto !== NEW_COMPANY
              ? mergeInto
              : undefined,
          reason: reason.trim() || undefined,
        },
        { etag: proposal.version, idempotencyKey: idemRef.current }
      );
      idemRef.current = idempotencyKey();
      toast.success(t("success"));
      await queryClient.invalidateQueries({ queryKey: ["lead-proposals"] });
      onOpenChange(false);
    } catch (err) {
      const apiErr = err instanceof ApiError ? err : null;
      if (apiErr?.status === 412 || apiErr?.status === 409) {
        setError(t("conflict"));
        void queryClient.invalidateQueries({ queryKey: ["lead-proposals"] });
      } else if (apiErr?.status === 428) {
        setError(t("versionPrecondition"));
      } else if (apiErr?.status === 403) {
        setError(t("forbidden"));
      } else {
        setError(apiErr?.problem.detail ?? t("failed"));
      }
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
            {decision === "accept" ? t("acceptTitle") : t("rejectTitle")}
          </DialogTitle>
          <DialogDescription>
            {decision === "accept"
              ? t("acceptDescription")
              : t("rejectDescription")}
          </DialogDescription>
        </DialogHeader>

        <div className="grid gap-4">
          {decision === "accept" && duplicates.length > 0 ? (
            <div className="grid gap-1.5">
              <Label htmlFor="merge-target">{t("mergeIntoLabel")}</Label>
              <Select value={mergeInto} onValueChange={setMergeInto}>
                <SelectTrigger id="merge-target" className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value={NEW_COMPANY}>
                    {t("mergeNew")}
                  </SelectItem>
                  {duplicates.map((companyId) => (
                    <SelectItem key={companyId} value={companyId}>
                      {t("mergeInto")} —{" "}
                      <span className="font-mono text-2xs">{companyId}</span>
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
          ) : null}

          <div className="grid gap-1.5">
            <Label htmlFor="decision-reason">{t("reason")}</Label>
            <Textarea
              id="decision-reason"
              rows={2}
              value={reason}
              placeholder={t("reasonPlaceholder")}
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
            variant={decision === "reject" ? "destructive" : "default"}
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
