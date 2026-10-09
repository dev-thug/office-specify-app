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
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  createSuppression,
  type SuppressionPurposeScope,
  type SuppressionReason,
} from "@/lib/api/compliance";
import { apiErrorMessage } from "./api-error-message";
import { useOperationKey } from "./use-operation-key";

const EMAIL_RE = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;

export interface SuppressionAddDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** 성공 후 목록 무효화 등. */
  onCreated?: () => void;
}

/**
 * 수동 차단 추가 (UI12 "차단 추가").
 * - 사유·목적 범위를 명시 — 서로 다른 차단 사유를 덮어쓰지 않는다 (§11.1).
 * - 멱등 키는 다이얼로그가 열려 있는 동안 유지하고, 확정 응답 후에만 재발급.
 */
export function SuppressionAddDialog({
  open,
  onOpenChange,
  onCreated,
}: SuppressionAddDialogProps) {
  const t = useTranslations("compliance.suppressions");
  const tc = useTranslations("common");
  const op = useOperationKey();

  const [address, setAddress] = useState("");
  const [scope, setScope] = useState<SuppressionPurposeScope>("marketing");
  const [reason, setReason] = useState<SuppressionReason>("manual");
  const [evidenceRef, setEvidenceRef] = useState("");
  const [error, setError] = useState<string | null>(null);

  const mutation = useMutation({
    mutationFn: (input: Parameters<typeof createSuppression>[0]) =>
      createSuppression(input, { idempotencyKey: op.key }),
    onSuccess: () => {
      op.settle(null);
      toast.success(t("add.successToast"));
      onOpenChange(false);
      onCreated?.();
    },
    onError: (e) => {
      op.settle(e);
      setError(apiErrorMessage(t, e));
    },
  });

  const trimmed = address.trim();
  const addressProblem =
    trimmed.length === 0
      ? null
      : EMAIL_RE.test(trimmed)
        ? null
        : t("addressInvalid");

  function submit(e: FormEvent) {
    e.preventDefault();
    setError(null);
    if (trimmed.length === 0) {
      setError(t("addressRequired"));
      return;
    }
    if (!EMAIL_RE.test(trimmed)) {
      setError(t("addressInvalid"));
      return;
    }
    mutation.mutate({
      address: trimmed,
      purpose_scope: scope,
      reason,
      ...(evidenceRef.trim() ? { evidence_ref: evidenceRef.trim() } : {}),
    });
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("add.title")}</DialogTitle>
          <DialogDescription>{t("add.description")}</DialogDescription>
        </DialogHeader>

        <form onSubmit={submit} className="grid gap-4" noValidate>
          <div className="grid gap-1.5">
            <Label htmlFor="suppression-address">{t("add.addressLabel")}</Label>
            <Input
              id="suppression-address"
              type="email"
              autoComplete="off"
              required
              value={address}
              onChange={(e) => setAddress(e.target.value)}
              placeholder={t("add.addressPlaceholder")}
              aria-invalid={addressProblem !== null}
              aria-describedby={
                addressProblem ? "suppression-address-error" : undefined
              }
            />
            {addressProblem ? (
              <p
                id="suppression-address-error"
                className="text-2xs text-destructive"
              >
                {addressProblem}
              </p>
            ) : null}
          </div>

          <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
            <div className="grid gap-1.5">
              <Label id="suppression-scope-label">{t("add.scopeLabel")}</Label>
              <Select
                value={scope}
                onValueChange={(v) => setScope(v as SuppressionPurposeScope)}
              >
                <SelectTrigger
                  aria-labelledby="suppression-scope-label"
                  className="w-full"
                >
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="marketing">{t("scope.marketing")}</SelectItem>
                  <SelectItem value="all">{t("scope.all")}</SelectItem>
                </SelectContent>
              </Select>
            </div>
            <div className="grid gap-1.5">
              <Label id="suppression-reason-label">{t("add.reasonLabel")}</Label>
              <Select
                value={reason}
                onValueChange={(v) => setReason(v as SuppressionReason)}
              >
                <SelectTrigger
                  aria-labelledby="suppression-reason-label"
                  className="w-full"
                >
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="unsubscribe">
                    {t("reason.unsubscribe")}
                  </SelectItem>
                  <SelectItem value="complaint">
                    {t("reason.complaint")}
                  </SelectItem>
                  <SelectItem value="hard_bounce">
                    {t("reason.hard_bounce")}
                  </SelectItem>
                  <SelectItem value="manual">{t("reason.manual")}</SelectItem>
                </SelectContent>
              </Select>
            </div>
          </div>

          <div className="grid gap-1.5">
            <Label htmlFor="suppression-evidence">
              {t("add.evidenceLabel")}
            </Label>
            <Input
              id="suppression-evidence"
              value={evidenceRef}
              onChange={(e) => setEvidenceRef(e.target.value)}
              placeholder={t("add.evidencePlaceholder")}
            />
            <p className="text-2xs text-muted-foreground">
              {t("add.evidenceHint")}
            </p>
          </div>

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
              disabled={mutation.isPending || trimmed.length === 0}
            >
              {mutation.isPending ? t("add.submitting") : t("add.submit")}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
