"use client";

import { useMutation } from "@tanstack/react-query";
import { useTranslations } from "next-intl";
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
import { Textarea } from "@/components/ui/textarea";
import {
  requestSuppressionRelease,
  type Suppression,
} from "@/lib/api/compliance";
import { apiErrorMessage } from "./api-error-message";
import { useOperationKey } from "./use-operation-key";

export interface SuppressionReleaseDialogProps {
  /** 대상 행 — null이면 닫힘. */
  suppression: Suppression | null;
  onOpenChange: (open: boolean) => void;
  onRequested?: () => void;
}

/**
 * 차단 해제 "요청" (UI12 "해제는 별도 승인") — POST release-requests는
 * 승인 요청을 만들 뿐 직접 해제하지 않는다. 다른 활성 사유가 남아 있으면
 * 해제 후에도 발송은 불가하다는 점을 문구로 명시한다 (§11.1).
 */
export function SuppressionReleaseDialog({
  suppression,
  onOpenChange,
  onRequested,
}: SuppressionReleaseDialogProps) {
  const t = useTranslations("compliance.suppressions");
  const tc = useTranslations("common");
  const op = useOperationKey();

  const [reason, setReason] = useState("");
  const [evidenceRef, setEvidenceRef] = useState("");
  const [error, setError] = useState<string | null>(null);

  const open = suppression !== null;

  const mutation = useMutation({
    mutationFn: (input: { reason: string; evidence_ref: string }) =>
      requestSuppressionRelease(suppression!.id, input, {
        idempotencyKey: op.key,
      }),
    onSuccess: () => {
      op.settle(null);
      toast.success(t("release.successToast"));
      onOpenChange(false);
      onRequested?.();
    },
    onError: (e) => {
      op.settle(e);
      setError(apiErrorMessage(t, e));
    },
  });

  function submit(e: FormEvent) {
    e.preventDefault();
    setError(null);
    if (!reason.trim() || !evidenceRef.trim()) return;
    mutation.mutate({
      reason: reason.trim(),
      evidence_ref: evidenceRef.trim(),
    });
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("release.title")}</DialogTitle>
          <DialogDescription>{t("release.description")}</DialogDescription>
        </DialogHeader>

        {suppression ? (
          <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5 rounded-2xl border border-border bg-app-panel-muted px-4 py-3 text-sm">
            <dt className="text-muted-foreground">
              {t("release.summaryAddress")}
            </dt>
            <dd className="font-medium">
              {suppression.email_address ?? suppression.email_address_id}
            </dd>
            <dt className="text-muted-foreground">
              {t("release.summaryScope")}
            </dt>
            <dd className="font-medium">
              {suppression.purpose_scope === "all"
                ? t("scope.all")
                : t("scope.marketing")}
            </dd>
            <dt className="text-muted-foreground">
              {t("release.summaryReason")}
            </dt>
            <dd className="font-medium">
              {t.has(`reason.${suppression.reason}`) ? t(`reason.${suppression.reason}`) : suppression.reason}
            </dd>
          </dl>
        ) : null}

        <form onSubmit={submit} className="grid gap-4">
          <div className="grid gap-1.5">
            <Label htmlFor="release-reason">{t("release.reasonLabel")}</Label>
            <Textarea
              id="release-reason"
              required
              value={reason}
              onChange={(e) => setReason(e.target.value)}
              placeholder={t("release.reasonPlaceholder")}
            />
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="release-evidence">
              {t("release.evidenceLabel")}
            </Label>
            <Input
              id="release-evidence"
              required
              value={evidenceRef}
              onChange={(e) => setEvidenceRef(e.target.value)}
              placeholder={t("release.evidencePlaceholder")}
            />
            <p className="text-2xs text-muted-foreground">
              {t("release.evidenceHint")}
            </p>
          </div>

          <p className="rounded-2xl border border-border bg-muted px-4 py-3 text-sm text-muted-foreground">
            {t("release.approvalNote")}
          </p>

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
                reason.trim().length === 0 ||
                evidenceRef.trim().length === 0
              }
            >
              {mutation.isPending
                ? t("release.submitting")
                : t("release.submit")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
