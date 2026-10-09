"use client";

import { useQueryClient } from "@tanstack/react-query";
import { useTranslations } from "next-intl";
import { useRef, useState } from "react";
import { toast } from "sonner";

import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import { cancelAgentRun, type AgentRun } from "@/lib/api/agents";
import { idempotencyKey } from "@/lib/api/client";
import { ApiError } from "@/lib/api/problem";

interface CancelRunDialogProps {
  run: AgentRun;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

/**
 * 실행 중단 확인 — 새 도구 실행만 막고 이미 제출된 외부 효과는 되돌리지 않는다
 * (B.2 agent-runs/{id}/cancel, §16.3). 되돌릴 수 없는 발송이 아니므로
 * ConfirmIrreversible 대신 AlertDialog + 사유 입력을 쓴다.
 */
export function CancelRunDialog({ run, open, onOpenChange }: CancelRunDialogProps) {
  const t = useTranslations("agents.detail");
  const tCommon = useTranslations("common");
  const queryClient = useQueryClient();
  const [reason, setReason] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const idemRef = useRef<string>(idempotencyKey());

  // 닫힐 때 이전 입력·오류를 지워 다음 열림이 항상 초기 상태가 되게 한다.
  function handleOpenChange(next: boolean) {
    if (!next) {
      setReason("");
      setError(null);
    }
    onOpenChange(next);
  }

  async function onConfirm() {
    setPending(true);
    setError(null);
    try {
      await cancelAgentRun(
        run.id,
        { reason: reason.trim() || undefined },
        { idempotencyKey: idemRef.current }
      );
      idemRef.current = idempotencyKey();
      toast.success(t("cancelSuccess"));
      await queryClient.invalidateQueries({ queryKey: ["agent-run", run.id] });
      await queryClient.invalidateQueries({ queryKey: ["agent-runs"] });
      handleOpenChange(false);
    } catch (err) {
      const apiErr = err instanceof ApiError ? err : null;
      setError(
        apiErr?.status === 403
          ? t("cancelForbidden")
          : (apiErr?.problem.detail ?? t("cancelFailed"))
      );
      if (apiErr && apiErr.problem.retryable !== true && apiErr.status < 500) {
        idemRef.current = idempotencyKey();
      }
    } finally {
      setPending(false);
    }
  }

  return (
    <AlertDialog open={open} onOpenChange={handleOpenChange}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{t("cancelTitle")}</AlertDialogTitle>
          <AlertDialogDescription>
            {t("cancelDescription")}
          </AlertDialogDescription>
        </AlertDialogHeader>
        <div className="grid gap-1.5">
          <Label htmlFor="cancel-run-reason">{t("cancelReason")}</Label>
          <Textarea
            id="cancel-run-reason"
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
        <AlertDialogFooter>
          <AlertDialogCancel disabled={pending}>
            {tCommon("cancel")}
          </AlertDialogCancel>
          <AlertDialogAction
            disabled={pending}
            onClick={(e) => {
              e.preventDefault();
              void onConfirm();
            }}
          >
            {pending ? t("canceling") : t("cancelConfirm")}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
