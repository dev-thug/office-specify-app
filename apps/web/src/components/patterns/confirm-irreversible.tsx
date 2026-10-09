"use client";

import { useTranslations } from "next-intl";
import { useState } from "react";

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
import { cn } from "@/lib/utils";

export interface ConfirmIrreversibleProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  description?: string;
  /** 실제 발송/승인 대상 — 수신자 수, 발신 주소, 목적, 내용 미리보기 (§7.2). */
  recipientsCount: number;
  /** 선택 사항 — 단건 작업처럼 정확한 수신 주소를 보여야 하는 경우 사용한다. */
  recipientAddresses?: string[];
  sender: string;
  purpose: string;
  contentPreview?: { subject?: string; body?: string };
  /** 이 문자열을 정확히 입력해야 확인 버튼이 활성화된다 (예: 수신자 수). */
  confirmText: string;
  confirmLabel?: string;
  destructive?: boolean;
  pending?: boolean;
  onConfirm: () => void;
}

/**
 * 되돌릴 수 없는 작업(발송·승인) 확인 대화상자 — 실제 수신자·발신 주소·
 * 목적·내용·건수를 확인시키고, 확인 문구 타이핑을 요구한다.
 */
export function ConfirmIrreversible({
  open,
  onOpenChange,
  title,
  description,
  recipientsCount,
  recipientAddresses,
  sender,
  purpose,
  contentPreview,
  confirmText,
  confirmLabel,
  destructive = true,
  pending = false,
  onConfirm,
}: ConfirmIrreversibleProps) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      {/* DialogContent 는 닫히면 언마운트된다 — 입력 상태를 내부 컴포넌트에 두어
          다시 열 때 자동으로 초기화한다. */}
      <ConfirmBody
        title={title}
        description={description}
        recipientsCount={recipientsCount}
        recipientAddresses={recipientAddresses}
        sender={sender}
        purpose={purpose}
        contentPreview={contentPreview}
        confirmText={confirmText}
        confirmLabel={confirmLabel}
        destructive={destructive}
        pending={pending}
        onConfirm={onConfirm}
        onCancel={() => onOpenChange(false)}
      />
    </Dialog>
  );
}

type ConfirmBodyProps = Omit<ConfirmIrreversibleProps, "open" | "onOpenChange"> & {
  onCancel: () => void;
};

function ConfirmBody({
  title,
  description,
  recipientsCount,
  recipientAddresses,
  sender,
  purpose,
  contentPreview,
  confirmText,
  confirmLabel,
  destructive = true,
  pending = false,
  onConfirm,
  onCancel,
}: ConfirmBodyProps) {
  const t = useTranslations("common.confirmIrreversible");
  const [typed, setTyped] = useState("");

  const confirmed = typed === confirmText;

  return (
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          {description ? (
            <DialogDescription>{description}</DialogDescription>
          ) : null}
        </DialogHeader>

        <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5 rounded-2xl border border-border bg-app-panel-muted px-4 py-3 text-sm">
          <dt className="text-muted-foreground">{t("recipients")}</dt>
          <dd className="font-medium">
            <span>{recipientsCount.toLocaleString()}</span>
            {recipientAddresses && recipientAddresses.length > 0 ? (
              <ul className="mt-1 grid gap-0.5 font-normal">
                {recipientAddresses.map((address) => (
                  <li key={address} className="break-all">{address}</li>
                ))}
              </ul>
            ) : null}
          </dd>
          <dt className="text-muted-foreground">{t("sender")}</dt>
          <dd className="font-medium">{sender}</dd>
          <dt className="text-muted-foreground">{t("purpose")}</dt>
          <dd className="font-medium">{purpose}</dd>
        </dl>

        {contentPreview ? (
          <div className="rounded-2xl border border-border px-4 py-3">
            {contentPreview.subject ? (
              <p className="text-sm font-medium">{contentPreview.subject}</p>
            ) : null}
            {contentPreview.body ? (
              <p className="mt-1 whitespace-pre-wrap text-sm text-muted-foreground">
                {contentPreview.body}
              </p>
            ) : null}
          </div>
        ) : null}

        <div className="grid gap-1.5">
          <Label htmlFor="confirm-irreversible-input" className="text-sm">
            {t("typeToConfirm", { text: confirmText })}
          </Label>
          <Input
            id="confirm-irreversible-input"
            value={typed}
            onChange={(e) => setTyped(e.target.value)}
            placeholder={confirmText}
            autoComplete="off"
            aria-invalid={typed.length > 0 && !confirmed}
            className={cn(typed.length > 0 && !confirmed && "border-destructive")}
          />
        </div>

        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            onClick={onCancel}
          >
            {t("cancel")}
          </Button>
          <Button
            type="button"
            variant={destructive ? "destructive" : "default"}
            disabled={!confirmed || pending}
            onClick={onConfirm}
          >
            {confirmLabel ?? t("confirm")}
          </Button>
        </DialogFooter>
      </DialogContent>
  );
}
