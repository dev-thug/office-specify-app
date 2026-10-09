"use client";

/**
 * CRM 보조 패널 (UI05) — 스레드에 연결된 담당자·회사 요약.
 * 이메일 형식 검증과 발송 허가는 서로 다른 배지로 구분한다 (UI04 규칙).
 * 차단 등록은 compliance 화면으로 이동하는 진입점 링크다.
 */
import { Ban, Building2, User } from "lucide-react";
import Link from "next/link";
import { useTranslations } from "next-intl";

import { StatusBadge } from "@/components/patterns/status-badge";
import { Badge } from "@/components/ui/badge";
import { buttonVariants } from "@/components/ui/button";
import { Separator } from "@/components/ui/separator";
import type { ThreadCrmContext } from "@/lib/api/inbox";
import { cn } from "@/lib/utils";
import { suppressionHref } from "./thread-state";

interface CrmPanelProps {
  crm: ThreadCrmContext | null | undefined;
  className?: string;
}

export function CrmPanel({ crm, className }: CrmPanelProps) {
  const t = useTranslations("inbox");
  const ts = useTranslations("status");

  const contact = crm?.contact ?? null;
  const company = crm?.company ?? null;

  return (
    <aside
      aria-label={t("detail.crm")}
      className={cn(
        "flex flex-col gap-3 rounded-2xl border border-border bg-card p-4",
        className
      )}
    >
      <h2 className="text-sm font-semibold">{t("detail.crm")}</h2>

      {contact ? (
        <div className="flex flex-col gap-2">
          <div className="flex items-center gap-2">
            <User aria-hidden className="size-4 text-muted-foreground" />
            <Link
              href={`/app/contacts/${contact.id}`}
              className="truncate text-sm font-medium text-brand underline-offset-4 hover:underline"
            >
              {contact.name}
            </Link>
          </div>
          {contact.title ? (
            <p className="text-xs text-muted-foreground">{contact.title}</p>
          ) : null}
          {contact.company_name ? (
            <p className="flex items-center gap-1.5 text-xs text-muted-foreground">
              <Building2 aria-hidden className="size-3.5" />
              {contact.company_name}
            </p>
          ) : null}

          {contact.email_addresses?.length ? (
            <ul className="mt-1 flex flex-col gap-2">
              {contact.email_addresses.map((email) => (
                <li
                  key={email.id ?? email.address}
                  className="flex flex-col gap-1 rounded-lg border border-border/60 px-2.5 py-2"
                >
                  <span className="truncate text-xs font-medium">
                    {email.address}
                    {email.is_primary ? (
                      <span className="ml-1.5 text-muted-foreground">
                        {t("crm.primary")}
                      </span>
                    ) : null}
                  </span>
                  <span className="flex flex-wrap items-center gap-1">
                    {/* 이메일 형식 유효 ≠ 발송 허가 — 배지를 구분한다. */}
                    <StatusBadge
                      status={email.is_verified ? "verified" : "unverified"}
                      label={
                        email.is_verified
                          ? t("crm.formatValid")
                          : t("crm.formatUnverified")
                      }
                    />
                    <StatusBadge
                      status={email.send_permitted ? "allowed" : "blocked"}
                      label={
                        email.send_permitted
                          ? t("crm.sendPermitted")
                          : t("crm.sendBlocked")
                      }
                    />
                    {email.suppressed ? (
                      <Badge variant="rose-subtle">
                        <Ban aria-hidden />
                        {ts("suppressed")}
                      </Badge>
                    ) : null}
                  </span>
                  <Link
                    href={suppressionHref(email.address)}
                    className={cn(
                      buttonVariants({ variant: "ghost", size: "xs" }),
                      "self-start text-muted-foreground"
                    )}
                  >
                    {t("detail.suppress")}
                  </Link>
                </li>
              ))}
            </ul>
          ) : null}
        </div>
      ) : (
        <p className="text-sm text-muted-foreground">{t("crm.noContact")}</p>
      )}

      {company ? (
        <>
          <Separator />
          <div className="flex flex-col gap-1.5">
            <Link
              href={`/app/companies/${company.id}`}
              className="truncate text-sm font-medium text-brand underline-offset-4 hover:underline"
            >
              {company.name}
            </Link>
            {company.domain ? (
              <p className="text-xs text-muted-foreground">{company.domain}</p>
            ) : null}
            {company.lifecycle_stage ? (
              <StatusBadge
                status={company.lifecycle_stage}
                className="self-start"
              />
            ) : null}
          </div>
        </>
      ) : null}
    </aside>
  );
}
