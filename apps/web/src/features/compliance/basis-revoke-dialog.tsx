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
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import {
  revokeCommunicationBasis,
  type CommunicationBasis,
} from "@/lib/api/compliance";
import { ApiError } from "@/lib/api/problem";
import { apiErrorMessage } from "./api-error-message";
import { useOperationKey } from "./use-operation-key";

export interface BasisRevokeDialogProps {
  basis: CommunicationBasis | null;
  onOpenChange: (open: boolean) => void;
  onSettled?: () => void;
}

/**
 * 근거 철회 (B.2 "철회·관련 발송 중단") — POST /communication-bases/{id}/revoke.
 * 철회하면 이 근거에 의존하는 발송이 중단된다는 점을 확인 문구로 보여준다.
 * If-Match로 본인이 본 버전을 철회하는지 보장한다.
 */
export function BasisRevokeDialog({
  basis,
  onOpenChange,
  onSettled,
}: BasisRevokeDialogProps) {
  const t = useTranslations("compliance.reviews");
  const tc = useTranslations("common");
  const op = useOperationKey();

  const [reason, setReason] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [conflict, setConflict] = useState(false);

  const open = basis !== null;

  const mutation = useMutation({
    mutationFn: (input: { reason: string }) =>
      revokeCommunicationBasis(basis!.id, input, {
        etag: basis!.version,
        idempotencyKey: op.key,
      }),
    onSuccess: () => {
      op.settle(null);
      toast.success(t("revoke.successToast"));
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
    if (reason.trim().length === 0) return;
    mutation.mutate({ reason: reason.trim() });
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("revoke.title")}</DialogTitle>
          <DialogDescription>{t("revoke.description")}</DialogDescription>
        </DialogHeader>

        {basis ? (
          <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5 rounded-2xl border border-border bg-app-panel-muted px-4 py-3 text-sm">
            <dt className="text-muted-foreground">{t("colAddress")}</dt>
            <dd className="font-medium">
              {basis.email_address ?? basis.email_address_id}
            </dd>
            <dt className="text-muted-foreground">{t("colPurpose")}</dt>
            <dd className="font-medium">{t.has(`purpose.${basis.purpose}`) ? t(`purpose.${basis.purpose}`) : basis.purpose}</dd>
            <dt className="text-muted-foreground">{t("colBasisType")}</dt>
            <dd className="font-medium">
              {t.has(`basisType.${basis.basis_type}`) ? t(`basisType.${basis.basis_type}`) : basis.basis_type}
            </dd>
          </dl>
        ) : null}

        <form onSubmit={submit} className="grid gap-4">
          <div className="grid gap-1.5">
            <Label htmlFor="basis-revoke-reason">
              {t("revoke.reasonLabel")}
              <span className="text-destructive"> *</span>
            </Label>
            <Textarea
              id="basis-revoke-reason"
              required
              value={reason}
              onChange={(e) => setReason(e.target.value)}
              placeholder={t("revoke.reasonPlaceholder")}
            />
          </div>

          {conflict ? (
            <Alert variant="warning">
              <AlertTitle>{t("conflictTitle")}</AlertTitle>
              <AlertDescription>
                <p>{t("conflict")}</p>
                <Button
                  type="button"
                  variant="outline"
                  size="xs"
                  onClick={() => {
                    onSettled?.();
                    onOpenChange(false);
                  }}
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
              variant="destructive"
              disabled={mutation.isPending || conflict || reason.trim().length === 0}
            >
              {mutation.isPending
                ? t("revoke.submitting")
                : t("revoke.submit")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
