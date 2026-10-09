"use client";

/**
 * OAuth 시작 URL 표시 대화상자 — 재연결/연결 요청의 응답을 한 번만 보여 준다.
 * 비밀 값이 아니라 브라우저 이동용 authorize URL만 다룬다 (secret 표시 금지, UI13).
 */
import { useTranslations } from "next-intl";

import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { CopyField } from "@/features/settings/copy-field";

interface ConnectUrlDialogProps {
  url: string | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

export function ConnectUrlDialog({ url, open, onOpenChange }: ConnectUrlDialogProps) {
  const t = useTranslations("operations.integrations.connectResult");
  const tCommon = useTranslations("common");
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t("title")}</DialogTitle>
          <DialogDescription>{t("description")}</DialogDescription>
        </DialogHeader>
        {url ? <CopyField value={url} label={t("copy")} openHref /> : null}
        <DialogFooter>
          <Button type="button" variant="outline" onClick={() => onOpenChange(false)}>
            {tCommon("close")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
