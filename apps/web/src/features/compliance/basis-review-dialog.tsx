"use client";

import { useMutation } from "@tanstack/react-query";
import { useFormatter, useTranslations } from "next-intl";
import { useState, type FormEvent } from "react";
import { toast } from "sonner";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
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
import {
  reviewCommunicationBasis,
  type BasisReviewDecision,
  type CommunicationBasis,
} from "@/lib/api/compliance";
import { ApiError } from "@/lib/api/problem";
import { apiErrorMessage } from "./api-error-message";
import { useOperationKey } from "./use-operation-key";

export interface BasisReviewDialogProps {
  basis: CommunicationBasis | null;
  onOpenChange: (open: boolean) => void;
  /** 성공·412 충돌 후 목록을 다시 읽게 한다. */
  onSettled?: () => void;
}

/**
 * 근거 검토 (UI12 "근거 검토") — POST /communication-bases/{id}/reviews.
 *
 * "checkbox로 동의 유효화 불가": 검토자가 증빙·수집 시각·관할을 확인하고
 * 명시적 판단(valid/rejected)과 근거를 남겨야 한다. 동의 확인용 빈 체크박스는
 * 두지 않는다.
 *
 * If-Match: 목록에서 읽은 version을 보낸다. 412 VERSION_MISMATCH는 누군가
 * 먼저 검토했다는 뜻 — 덮어쓰지 않고 새로고침을 유도한다.
 */
export function BasisReviewDialog({
  basis,
  onOpenChange,
  onSettled,
}: BasisReviewDialogProps) {
  const t = useTranslations("compliance.reviews");
  const tc = useTranslations("common");
  const format = useFormatter();
  const op = useOperationKey();

  const [decision, setDecision] = useState<BasisReviewDecision>("valid");
  const [validUntil, setValidUntil] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [conflict, setConflict] = useState(false);

  const open = basis !== null;

  const mutation = useMutation({
    mutationFn: (input: {
      decision: BasisReviewDecision;
      valid_until?: string;
    }) =>
      reviewCommunicationBasis(basis!.id, input, {
        // ETag는 GET 응답 헤더의 "v" — 리소스 version 필드를 그대로 쓴다 (§4.4).
        etag: basis!.version,
        idempotencyKey: op.key,
      }),
    onSuccess: () => {
      op.settle(null);
      toast.success(t("review.successToast"));
      onOpenChange(false);
      onSettled?.();
    },
    onError: (e) => {
      op.settle(e);
      if (e instanceof ApiError && e.problem.status === 412) {
        setConflict(true);
        setError(null);
      } else {
        setError(apiErrorMessage(t, e));
      }
    },
  });

  function submit(e: FormEvent) {
    e.preventDefault();
    setError(null);
    mutation.mutate({
      decision,
      ...(validUntil ? { valid_until: new Date(validUntil).toISOString() } : {}),
    });
  }

  function refreshAndClose() {
    onSettled?.();
    onOpenChange(false);
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("review.title")}</DialogTitle>
          <DialogDescription>{t("review.description")}</DialogDescription>
        </DialogHeader>

        {basis ? (
          <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5 rounded-2xl border border-border bg-app-panel-muted px-4 py-3 text-sm">
            <dt className="text-muted-foreground">{t("colAddress")}</dt>
            <dd className="font-medium">
              {basis.email_address ?? basis.email_address_id}
            </dd>
            <dt className="text-muted-foreground">{t("colPurpose")}</dt>
            <dd className="font-medium">
              {t.has(`purpose.${basis.purpose}`) ? t(`purpose.${basis.purpose}`) : basis.purpose}
            </dd>
            <dt className="text-muted-foreground">{t("colBasisType")}</dt>
            <dd className="font-medium">
              {t.has(`basisType.${basis.basis_type}`) ? t(`basisType.${basis.basis_type}`) : basis.basis_type}
            </dd>
            <dt className="text-muted-foreground">{t("colJurisdiction")}</dt>
            <dd className="font-medium">{basis.jurisdiction}</dd>
            <dt className="text-muted-foreground">{t("colCollectedAt")}</dt>
            <dd className="font-medium">
              {format.dateTime(new Date(basis.collected_at), {
                dateStyle: "medium",
                timeStyle: "short",
              })}
            </dd>
          </dl>
        ) : null}

        {basis ? (
          <div className="grid gap-1.5">
            <span className="text-sm font-medium">{t("review.evidenceLabel")}</span>
            <p className="whitespace-pre-wrap break-all rounded-2xl border border-border px-4 py-3 font-mono text-sm">
              {basis.evidence_ref}
            </p>
            <p className="text-2xs text-muted-foreground">
              {t("review.evidenceHint")}
            </p>
          </div>
        ) : null}

        <form onSubmit={submit} className="grid gap-4">
          <div className="grid gap-1.5">
            <Label id="basis-decision-label">{t("review.decisionLabel")}</Label>
            <Select
              value={decision}
              onValueChange={(v) => setDecision(v as BasisReviewDecision)}
            >
              <SelectTrigger
                aria-labelledby="basis-decision-label"
                className="w-full"
              >
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="valid">
                  {t("review.decisionValid")}
                </SelectItem>
                <SelectItem value="rejected">
                  {t("review.decisionRejected")}
                </SelectItem>
              </SelectContent>
            </Select>
          </div>

          {decision === "valid" ? (
            <div className="grid gap-1.5">
              <Label htmlFor="basis-valid-until">
                {t("review.validUntilLabel")}
              </Label>
              <Input
                id="basis-valid-until"
                type="datetime-local"
                value={validUntil}
                onChange={(e) => setValidUntil(e.target.value)}
              />
              <p className="text-2xs text-muted-foreground">
                {t("review.validUntilHint")}
              </p>
            </div>
          ) : null}

          {conflict ? (
            <Alert variant="warning">
              <AlertTitle>{t("conflictTitle")}</AlertTitle>
              <AlertDescription>
                <p>{t("conflict")}</p>
                <Button
                  type="button"
                  variant="outline"
                  size="xs"
                  onClick={refreshAndClose}
                >
                  {t("conflictRefresh")}
                </Button>
              </AlertDescription>
            </Alert>
          ) : null}

          {error ? (
            <Alert variant="destructive">
              <AlertTitle>{t("errors.title")}</AlertTitle>
              <AlertDescription>{error}</AlertDescription>
            </Alert>
          ) : null}

          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => onOpenChange(false)}
            >
              {tc("cancel")}
            </Button>
            <Button
              type="submit"
              disabled={
                mutation.isPending ||
                conflict
              }
            >
              {mutation.isPending
                ? t("review.submitting")
                : t("review.submit")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
