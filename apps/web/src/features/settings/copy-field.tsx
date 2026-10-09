"use client";

/**
 * 일회성 값(초대 링크·credential secret·OAuth URL)의 읽기 전용 표시 + 복사.
 * 값은 대화상자를 닫으면 다시 볼 수 없다 — 안내 문구는 호출부 메시지로 둔다.
 */
import { CheckIcon, CopyIcon, ExternalLinkIcon } from "lucide-react";
import { useTranslations } from "next-intl";
import { useState } from "react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";

interface CopyFieldProps {
  value: string;
  /** 복사 대상 설명(aria-label·스크린리더용). */
  label: string;
  /** 외부 URL이면 새 탭으로 열기 버튼을 함께 보인다. */
  openHref?: boolean;
  className?: string;
}

export function CopyField({ value, label, openHref, className }: CopyFieldProps) {
  const t = useTranslations("settings.common");
  const [copied, setCopied] = useState(false);

  async function copy() {
    try {
      await navigator.clipboard.writeText(value);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch {
      // 클립보드 API 실패 — 수동 복사할 수 있게 입력란은 그대로 둔다.
    }
  }

  return (
    <div className={cn("flex items-center gap-2", className)}>
      <Input
        readOnly
        value={value}
        aria-label={label}
        onFocus={(e) => e.target.select()}
        className="font-mono text-xs"
      />
      {openHref ? (
        <Button type="button" variant="outline" size="sm" asChild>
          <a href={value} target="_blank" rel="noopener noreferrer">
            <ExternalLinkIcon aria-hidden />
            {t("open")}
          </a>
        </Button>
      ) : null}
      <Button type="button" variant="outline" size="sm" onClick={copy}>
        {copied ? <CheckIcon aria-hidden /> : <CopyIcon aria-hidden />}
        {copied ? t("copied") : t("copy")}
      </Button>
    </div>
  );
}
