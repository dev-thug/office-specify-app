"use client";

/**
 * 메일함 재연결 경고 배너 — 재인증 필요/비활성 메일함은 동기·발송이 멈추므로
 * 최상단에 차단 수준으로 표시한다 (UI05, §7.3 규칙).
 */
import { PlugZap } from "lucide-react";
import Link from "next/link";
import { useTranslations } from "next-intl";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { buttonVariants } from "@/components/ui/button";
import type { MailAccount } from "@/lib/api/inbox";
import { cn } from "@/lib/utils";

interface ReconnectBannerProps {
  accounts: MailAccount[];
  /** 최상단 차단 배너 스타일(warning). false 면 보조 info. */
  blocking?: boolean;
  className?: string;
}

export function ReconnectBanner({
  accounts,
  blocking = true,
  className,
}: ReconnectBannerProps) {
  const t = useTranslations("inbox.reconnect");
  if (accounts.length === 0) return null;

  const names = accounts
    .map((a) => a.name || a.primary_address)
    .join(", ");

  return (
    <Alert variant={blocking ? "warning" : "info"} className={className}>
      <PlugZap aria-hidden />
      <AlertTitle>{t("title")}</AlertTitle>
      <AlertDescription>
        <p>{t("description", { count: accounts.length, names })}</p>
        <Link
          href="/app/settings/mail-accounts"
          className={cn(buttonVariants({ variant: "outline", size: "sm" }))}
        >
          {t("action")}
        </Link>
      </AlertDescription>
    </Alert>
  );
}
