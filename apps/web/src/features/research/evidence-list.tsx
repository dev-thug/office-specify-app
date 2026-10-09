"use client";

import { useFormatter, useTranslations } from "next-intl";
import { ExternalLink } from "lucide-react";
import type { ReactNode } from "react";

import { StatusBadge } from "@/components/patterns/status-badge";
import { Badge } from "@/components/ui/badge";
import type { ResearchEvidence } from "@/lib/api/research";

import { displayValue } from "./proposal-utils";

/**
 * 필드별 리서치 증빙 표시 — 출처 URL·관측 시각·사실/추론 구분·모델 확신도·
 * 사람 검토 상태를 함께 보여준다 (§13.4). 확신도는 검증 완료를 대신하지
 * 않으므로 항상 낮은 강조로 표시한다.
 */
/**
 * 에이전트가 수집한 출처 URL은 불신 데이터다 (§13.5) — http/https 만 링크로
 * 만들고 javascript: 등 다른 스킴은 원문 문자열만 텍스트로 보여준다.
 */
export function isSafeEvidenceUrl(url: string): boolean {
  try {
    const protocol = new URL(url).protocol;
    return protocol === "http:" || protocol === "https:";
  } catch {
    return false;
  }
}

export function EvidenceItem({
  evidence,
  actions,
}: {
  evidence: ResearchEvidence;
  /** 검토 버튼 등 우측 액션 슬롯 (리드 후보 안의 읽기 전용 표시는 생략). */
  actions?: ReactNode;
}) {
  const t = useTranslations("research.evidence");
  const format = useFormatter();
  const safeUrl = isSafeEvidenceUrl(evidence.source_url);

  return (
    <div className="flex flex-col gap-2 rounded-2xl border border-border px-4 py-3">
      <div className="flex flex-wrap items-center gap-2">
        <span className="font-mono text-ui font-medium">
          {evidence.field_path}
        </span>
        <Badge
          variant={
            evidence.evidence_type === "observed" ? "info-subtle" : "amber-subtle"
          }
        >
          {evidence.evidence_type === "observed"
            ? t("typeObserved")
            : t("typeInferred")}
        </Badge>
        <StatusBadge status={evidence.review_status} />
        {evidence.confidence !== null ? (
          <span className="text-2xs text-muted-foreground">
            {t("confidence", {
              value: Math.round(evidence.confidence * 100),
            })}
          </span>
        ) : null}
        {actions ? <span className="ms-auto flex gap-1.5">{actions}</span> : null}
      </div>

      <p className="break-words text-sm">{displayValue(evidence.value)}</p>

      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-2xs text-muted-foreground">
        {safeUrl ? (
          <a
            href={evidence.source_url}
            target="_blank"
            rel="noreferrer noopener"
            className="inline-flex max-w-full items-center gap-1 truncate text-brand underline-offset-2 hover:underline"
          >
            <ExternalLink className="size-3 shrink-0" aria-hidden />
            <span className="truncate">{evidence.source_url}</span>
          </a>
        ) : (
          <span className="max-w-full truncate">
            {evidence.source_url}
          </span>
        )}
        <span>
          {t("observedAt")}:{" "}
          {format.dateTime(new Date(evidence.observed_at), {
            dateStyle: "medium",
            timeStyle: "short",
          })}
        </span>
        {evidence.reviewed_by ? (
          <span>
            {t("reviewedBy")}:{" "}
            <span className="font-mono">{evidence.reviewed_by}</span>
          </span>
        ) : null}
      </div>

      {evidence.excerpt ? (
        <blockquote className="border-s-2 border-border ps-3 text-sm text-muted-foreground">
          {evidence.excerpt}
        </blockquote>
      ) : null}
    </div>
  );
}

/** 증빙 목록 — 비어 있으면 안내 문구를 표시한다. */
export function EvidenceList({
  evidence,
  actions,
}: {
  evidence: ResearchEvidence[];
  /** 항목별 액션 슬롯 생성기. */
  actions?: (item: ResearchEvidence) => ReactNode;
}) {
  const t = useTranslations("research.evidence");
  if (evidence.length === 0) {
    return (
      <p className="text-sm text-muted-foreground">{t("empty")}</p>
    );
  }
  return (
    <div className="flex flex-col gap-2">
      {evidence.map((item) => (
        <EvidenceItem
          key={item.id}
          evidence={item}
          actions={actions?.(item)}
        />
      ))}
    </div>
  );
}
