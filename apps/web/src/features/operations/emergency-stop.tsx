"use client";

/**
 * 긴급 중지 토글 (PUT /operations/emergency-stop — enabled·이유·If-Match).
 * 되돌릴 수는 있지만 영향이 즉시 전체 워크스페이스에 미치므로
 * 사유 입력 + 확인 문구 타이핑을 요구한다 (UI13 + §15.3).
 * 공용 ConfirmIrreversible 은 발송용 필드(수신자/발신자)만 받으므로
 * 여기서는 동일 강도의 전용 확인 대화상자를 쓴다.
 */
import { OctagonXIcon, ShieldAlertIcon } from "lucide-react";
import { useLocale, useTranslations } from "next-intl";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useRef, useState, type FormEvent } from "react";

import { ErrorState } from "@/components/patterns/error-state";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
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
import { Skeleton } from "@/components/ui/skeleton";
import { Textarea } from "@/components/ui/textarea";
import { idempotencyKey } from "@/lib/api/client";
import { ApiError } from "@/lib/api/problem";
import {
  getEmergencyStop,
  putEmergencyStop,
  type EmergencyStopState,
} from "@/lib/api/operations";
import { emergencyStopFormValid } from "@/features/operations/gate-rules";
import { apiErrorText } from "@/features/settings/api-error";
import { formatDateTime } from "@/features/settings/format";
import { useIsAdmin } from "@/features/settings/use-me";

export function EmergencyStopCard() {
  const t = useTranslations("operations.emergencyStop");
  const tOps = useTranslations("operations");
  const locale = useLocale();
  const isAdmin = useIsAdmin();
  const queryClient = useQueryClient();
  const [dialog, setDialog] = useState<"enable" | "disable" | null>(null);

  const state = useQuery({
    queryKey: ["operations", "emergency-stop"],
    queryFn: getEmergencyStop,
  });

  if (state.isPending) {
    return <Skeleton className="h-24 rounded-2xl" aria-busy="true" />;
  }
  if (state.isError || !state.data) {
    return (
      <ErrorState
        error={state.error}
        onRetry={() => state.refetch()}
        className="py-6"
      />
    );
  }

  const s: EmergencyStopState = state.data.data;
  const nextDialog = s.enabled ? "disable" : "enable";

  return (
    <Card className={s.enabled ? "border-amber/40" : undefined}>
      <CardHeader className="pb-2">
        <CardTitle className="flex flex-wrap items-center gap-2 text-base">
          <ShieldAlertIcon className="size-4" aria-hidden />
          {t("title")}
          <Badge variant={s.enabled ? "amber-subtle" : "secondary"}>
            {s.enabled ? t("status.enabled") : t("status.disabled")}
          </Badge>
        </CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <p className="text-sm text-muted-foreground">{t("description")}</p>
        <dl className="flex flex-wrap gap-x-4 gap-y-1 text-2xs text-muted-foreground">
          <div className="flex gap-1">
            <dt>{t("reasonLabel")}:</dt>
            <dd>{s.reason ?? "—"}</dd>
          </div>
          {s.updated_at ? (
            <div className="flex gap-1">
              <dt className="sr-only">updated</dt>
              <dd>{t("updatedAt", { time: formatDateTime(s.updated_at, locale) })}</dd>
            </div>
          ) : null}
          <div className="flex gap-1">
            <dt className="sr-only">version</dt>
            <dd>{t("version", { version: s.version })}</dd>
          </div>
        </dl>
        <div className="flex items-center gap-2">
          {isAdmin ? (
            <Button
              type="button"
              variant={s.enabled ? "outline" : "destructive"}
              size="sm"
              onClick={() => setDialog(nextDialog)}
            >
              <OctagonXIcon aria-hidden />
              {s.enabled ? t("disable") : t("enable")}
            </Button>
          ) : (
            <p className="text-2xs text-muted-foreground">{t("readOnly")}</p>
          )}
        </div>
      </CardContent>
      <EmergencyStopDialog
        mode={nextDialog}
        version={s.version}
        open={dialog !== null}
        onOpenChange={(open) => setDialog(open ? nextDialog : null)}
        onDone={() => {
          setDialog(null);
          queryClient.invalidateQueries({ queryKey: ["operations", "emergency-stop"] });
        }}
        errorText={(err) => apiErrorText(tOps, err)}
      />
    </Card>
  );
}

interface DialogProps {
  mode: "enable" | "disable";
  version: number;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onDone: () => void;
  errorText: (err: unknown) => string;
}

function EmergencyStopDialog({
  mode,
  version,
  open,
  onOpenChange,
  onDone,
  errorText,
}: DialogProps) {
  const t = useTranslations("operations.emergencyStop");
  const confirmWord = t(`confirmWord.${mode}`);
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <EmergencyStopDialogBody
        mode={mode}
        version={version}
        confirmWord={confirmWord}
        onCancel={() => onOpenChange(false)}
        onDone={onDone}
        errorText={errorText}
      />
    </Dialog>
  );
}

function EmergencyStopDialogBody({
  mode,
  version,
  confirmWord,
  onCancel,
  onDone,
  errorText,
}: Omit<DialogProps, "open" | "onOpenChange"> & {
  confirmWord: string;
  onCancel: () => void;
}) {
  const t = useTranslations("operations.emergencyStop");
  const tCommon = useTranslations("common");
  const [reason, setReason] = useState("");
  const [typed, setTyped] = useState("");
  const [error, setError] = useState<string | null>(null);
  // 논리 작업(이 토글 시도)당 멱등 키 하나 — DialogContent는 닫히면 언마운트되므로
  // ref 가 아닌 body 생명주기에 묶는다.
  const idemRef = useRef(idempotencyKey());

  const mutation = useMutation({
    mutationFn: () =>
      putEmergencyStop(
        { enabled: mode === "enable", reason: reason.trim() },
        { etag: version, idempotencyKey: idemRef.current }
      ),
    onSuccess: onDone,
    onError: (err) => {
      setError(errorText(err));
      if (err instanceof ApiError && err.problem.retryable !== true && err.status < 500) {
        idemRef.current = idempotencyKey();
      }
    },
  });

  const valid = emergencyStopFormValid(reason, typed, confirmWord);

  function onSubmit(e: FormEvent<HTMLFormElement>) {
    e.preventDefault();
    if (!valid || mutation.isPending) return;
    mutation.mutate();
  }

  return (
    <DialogContent>
      <DialogHeader>
        <DialogTitle>
          {mode === "enable" ? t("confirmTitleEnable") : t("confirmTitleDisable")}
        </DialogTitle>
        <DialogDescription>
          {mode === "enable" ? t("confirmDescEnable") : t("confirmDescDisable")}
        </DialogDescription>
      </DialogHeader>
      <form onSubmit={onSubmit} className="flex flex-col gap-4">
        <div className="grid gap-1.5">
          <Label htmlFor="emergency-stop-reason">{t("reason")}</Label>
          <Textarea
            id="emergency-stop-reason"
            required
            value={reason}
            onChange={(e) => setReason(e.target.value)}
            placeholder={t("reasonPlaceholder")}
            rows={2}
          />
        </div>
        <div className="grid gap-1.5">
          <Label htmlFor="emergency-stop-confirm">
            {t("typeToConfirm", { word: confirmWord })}
          </Label>
          <Input
            id="emergency-stop-confirm"
            value={typed}
            onChange={(e) => setTyped(e.target.value)}
            placeholder={confirmWord}
            autoComplete="off"
            aria-invalid={typed.length > 0 && typed !== confirmWord}
          />
        </div>
        {error ? (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        ) : null}
        <DialogFooter>
          <Button type="button" variant="outline" onClick={onCancel}>
            {tCommon("cancel")}
          </Button>
          <Button
            type="submit"
            variant={mode === "enable" ? "destructive" : "default"}
            disabled={!valid || mutation.isPending}
          >
            {t(`confirmButton.${mode}`)}
          </Button>
        </DialogFooter>
      </form>
    </DialogContent>
  );
}
