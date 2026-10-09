"use client";

import { useMutation } from "@tanstack/react-query";
import { useTranslations } from "next-intl";
import { useMemo, useRef, useState } from "react";
import { CheckIcon, CopyIcon } from "lucide-react";
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
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { ApiError } from "@/lib/api/problem";
import { idempotencyKey } from "@/lib/api/client";
import {
  createBookingLink,
  MEETING_TYPES_PATH,
  type BookingLink,
  type MeetingType,
} from "@/lib/api/meetings";
import { useCursorList } from "@/lib/api/use-cursor-list";
import { meetingsErrorMessage } from "@/features/meetings/error-message";

interface BookingLinkDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** 미리 선택할 유형 — 미팅 유형 목록 행에서 열 때 넘긴다. */
  presetTypeId?: string;
}

/**
 * 예약 링크 발급 — POST /booking-links. 응답의 URL/토큰은 이 대화상자에서만
 * 보인다(서버는 hash만 저장) — 발급 후 즉시 복사하도록 안내한다.
 */
export function BookingLinkDialog({
  open,
  onOpenChange,
  presetTypeId,
}: BookingLinkDialogProps) {
  const t = useTranslations("meetings");
  const tCommon = useTranslations("common");
  const [typeId, setTypeId] = useState<string | undefined>(presetTypeId);
  const [maxUses, setMaxUses] = useState("1");
  const [expires, setExpires] = useState<"never" | "7" | "30">("never");
  const [issued, setIssued] = useState<BookingLink | null>(null);
  const [copied, setCopied] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const idemRef = useRef(idempotencyKey());

  const typesQuery = useCursorList<MeetingType>({
    queryKey: ["meetings", "types", "active"],
    path: MEETING_TYPES_PATH,
    params: { status: "active" },
    enabled: open,
  });

  const types = useMemo(
    () => (typesQuery.data?.pages.flatMap((p) => p.data) ?? []).filter((meetingType) => meetingType.status === "active"),
    [typesQuery.data]
  );

  const mutation = useMutation({
    mutationFn: () =>
      createBookingLink(
        {
          meeting_type_id: typeId!,
          max_uses: Math.max(1, Number.parseInt(maxUses, 10) || 1),
          ...(expires !== "never"
            ? {
                expires_at: new Date(
                  Date.now() + Number(expires) * 24 * 60 * 60 * 1000
                ).toISOString(),
              }
            : {}),
        },
        { idempotencyKey: idemRef.current }
      ),
    onSuccess: (res) => {
      setIssued(res.data);
      setError(null);
    },
    onError: (err) => {
      setError(meetingsErrorMessage(err, (k) => t(`errors.${k}`)));
      if (
        err instanceof ApiError &&
        err.problem.retryable !== true &&
        err.status < 500
      ) {
        idemRef.current = idempotencyKey();
      }
    },
  });

  // 응답의 공개 URL — 서버가 url/booking_url 중 무엇을 주든, 없으면 token으로 조립한다.
  const issuedUrl = issued
    ? (issued.url ??
      issued.token_url ??
      issued.booking_url ??
      (issued.token
        ? `${window.location.origin}/public/book/${issued.token}`
        : undefined))
    : undefined;

  const close = (next: boolean) => {
    if (!next) {
      // 다시 열 때 발급 결과는 남기지 않는다 — 토큰 일회성 노출 정책.
      setIssued(null);
      setCopied(false);
      setError(null);
      idemRef.current = idempotencyKey();
    }
    onOpenChange(next);
  };

  return (
    <Dialog open={open} onOpenChange={close}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("link.title")}</DialogTitle>
          <DialogDescription>{t("link.description")}</DialogDescription>
        </DialogHeader>

        {issued ? (
          <div className="flex flex-col gap-3">
            <p className="text-sm font-medium">{t("link.createdTitle")}</p>
            {issuedUrl ? (
              <div className="flex items-center gap-2">
                <Input readOnly value={issuedUrl} aria-label="booking link" />
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={async () => {
                    try {
                      await navigator.clipboard.writeText(issuedUrl);
                      setCopied(true);
                      toast.success(t("link.copied"));
                    } catch {
                      setCopied(false);
                    }
                  }}
                >
                  {copied ? (
                    <CheckIcon className="size-4" aria-hidden />
                  ) : (
                    <CopyIcon className="size-4" aria-hidden />
                  )}
                  {copied ? t("link.copied") : t("link.copy")}
                </Button>
              </div>
            ) : (
              <p role="alert" className="text-sm text-destructive">
                {t("link.urlMissing")}
              </p>
            )}
            <p className="text-2xs text-muted-foreground">
              {t("link.createdBody")}
            </p>
          </div>
        ) : (
          <div className="flex flex-col gap-4">
            <div className="grid gap-1.5">
              <Label htmlFor="link-type">{t("link.meetingType")}</Label>
              <Select value={typeId} onValueChange={setTypeId}>
                <SelectTrigger id="link-type" className="w-full">
                  <SelectValue placeholder={t("link.selectType")} />
                </SelectTrigger>
                <SelectContent>
                  {types.map((mt) => (
                    <SelectItem key={mt.id} value={mt.id}>
                      {mt.name} · {t("types.minutes", { count: mt.duration_minutes })}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              {!typesQuery.isPending && types.length === 0 ? (
                <p className="text-2xs text-muted-foreground">
                  {t("link.noTypes")}
                </p>
              ) : null}
            </div>
            <div className="grid grid-cols-2 gap-3">
              <div className="grid gap-1.5">
                <Label htmlFor="link-max-uses">{t("link.maxUses")}</Label>
                <Input
                  id="link-max-uses"
                  type="number"
                  min={1}
                  value={maxUses}
                  onChange={(e) => setMaxUses(e.target.value)}
                />
              </div>
              <div className="grid gap-1.5">
                <Label htmlFor="link-expires">{t("link.expires")}</Label>
                <Select
                  value={expires}
                  onValueChange={(v) => setExpires(v as typeof expires)}
                >
                  <SelectTrigger id="link-expires" className="w-full">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="never">{t("link.expiresNever")}</SelectItem>
                    <SelectItem value="7">{t("link.expires7")}</SelectItem>
                    <SelectItem value="30">{t("link.expires30")}</SelectItem>
                  </SelectContent>
                </Select>
              </div>
            </div>
            {error ? (
              <p role="alert" className="text-sm text-destructive">
                {error}
              </p>
            ) : null}
          </div>
        )}

        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            onClick={() => close(false)}
          >
            {tCommon("close")}
          </Button>
          {!issued ? (
            <Button
              type="button"
              disabled={!typeId || mutation.isPending}
              onClick={() => mutation.mutate()}
            >
              {mutation.isPending ? t("link.issuing") : t("link.issue")}
            </Button>
          ) : null}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
