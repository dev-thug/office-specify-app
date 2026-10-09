"use client";

/**
 * 분류 검토 대화상자 (§9.4) — 모델 분류와 사람의 검토를 구분한다.
 * unsubscribe_request 선택 시 차단 등록 진입점을 함께 보여준다 —
 * 모호한 회신은 계속 발송 대신 안전하게 검토 대기시키는 규칙의 UI 측 구현.
 */
import Link from "next/link";
import { useTranslations } from "next-intl";
import { useState } from "react";
import { toast } from "sonner";

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
import { Textarea } from "@/components/ui/textarea";
import { idempotencyKey } from "@/lib/api/client";
import {
  createClassificationReview,
  MESSAGE_CLASSIFICATIONS,
  type EmailMessage,
  type MessageClassification,
} from "@/lib/api/inbox";
import { ApiError } from "@/lib/api/problem";
import {
  inboxErrorKey,
  participantAddresses,
  suppressionHref,
} from "./thread-state";

interface ClassificationReviewDialogProps {
  message: EmailMessage | null;
  onOpenChange: (open: boolean) => void;
  onReviewed: () => void;
}

export function ClassificationReviewDialog({
  message,
  onOpenChange,
  onReviewed,
}: ClassificationReviewDialogProps) {
  const t = useTranslations("inbox");
  const tc = useTranslations("common");
  const [classification, setClassification] =
    useState<MessageClassification>("unknown");
  const [reason, setReason] = useState("");
  const [pending, setPending] = useState(false);
  const [initializedFor, setInitializedFor] = useState<string | null>(null);
  // 논리 작업당 멱등 키 — 메시지가 바뀌거나 확정적 실패 후에만 재발급한다.
  const [opKey, setOpKey] = useState<string | null>(null);

  const open = message !== null;

  // 메시지가 바뀌면 현재 분류값으로 초기화한다 (열 때마다) — 렌더 중 상태 조정.
  if (message && initializedFor !== message.id) {
    setInitializedFor(message.id);
    setClassification(message.classification);
    setReason("");
    setOpKey(idempotencyKey());
  }

  async function submit() {
    if (!message) return;
    const key = opKey ?? idempotencyKey();
    if (!opKey) setOpKey(key);
    setPending(true);
    try {
      await createClassificationReview(
        message.id,
        {
          classification,
          reason: reason.trim() === "" ? undefined : reason.trim(),
        },
        { idempotencyKey: key }
      );
      toast.success(t("review.success"));
      onReviewed();
      onOpenChange(false);
    } catch (error) {
      if (error instanceof ApiError) {
        const errorKey = inboxErrorKey(error.problem);
        toast.error(
          errorKey
            ? t(`errors.${errorKey}`)
            : (error.problem.detail ?? error.message)
        );
        // 재시도 불가(<500) 오류 후에는 새 논리 작업으로 간주해 키 재발급.
        if (error.problem.retryable !== true && error.status < 500) {
          setOpKey(idempotencyKey());
        }
      } else {
        toast.error(t("errors.title"));
      }
    } finally {
      setPending(false);
    }
  }

  const fromAddress = message
    ? (participantAddresses(message, "from")[0] ?? null)
    : null;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("review.title")}</DialogTitle>
          <DialogDescription>{t("review.description")}</DialogDescription>
        </DialogHeader>

        <div className="grid gap-1.5">
          <Label id="classification-review-select-label">
            {t("review.fieldLabel")}
          </Label>
          <Select
            value={classification}
            onValueChange={(v) => setClassification(v as MessageClassification)}
          >
            <SelectTrigger
              aria-labelledby="classification-review-select-label"
              className="w-full"
            >
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {MESSAGE_CLASSIFICATIONS.map((value) => (
                <SelectItem key={value} value={value}>
                  {t(`classifications.${value}`)}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>

        {classification === "unsubscribe_request" && fromAddress ? (
          <p className="text-sm text-muted-foreground">
            {t("review.suppressionHint")}{" "}
            <Link
              href={suppressionHref(fromAddress)}
              className="font-medium text-brand underline-offset-4 hover:underline"
            >
              {t("detail.suppress")}
            </Link>
          </p>
        ) : null}

        <div className="grid gap-1.5">
          <Label htmlFor="classification-review-reason">
            {t("review.reasonLabel")}
          </Label>
          <Textarea
            id="classification-review-reason"
            value={reason}
            onChange={(e) => setReason(e.target.value)}
            placeholder={t("review.reasonPlaceholder")}
            rows={3}
          />
        </div>

        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            onClick={() => onOpenChange(false)}
          >
            {tc("cancel")}
          </Button>
          <Button type="button" disabled={pending} onClick={submit}>
            {pending ? tc("loading") : t("review.submit")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
