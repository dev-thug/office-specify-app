"use client";

import { OctagonX } from "lucide-react";
import { useTranslations } from "next-intl";

import { ApiError } from "@/lib/api/problem";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

interface ErrorStateProps {
  title?: string;
  /** ApiError 를 넘기면 코드·요청 ID를 함께 표시한다. */
  error?: ApiError | Error | null;
  description?: string;
  onRetry?: () => void;
  retryLabel?: string;
  className?: string;
}

/** 오류 상태 — request_id 를 항상 노출해 지원·감사 추적을 가능하게 한다 (§7.2). */
export function ErrorState({
  title,
  error,
  description,
  onRetry,
  retryLabel,
  className,
}: ErrorStateProps) {
  const t = useTranslations("common");
  const apiError = error instanceof ApiError ? error : null;
  const message = apiError?.problem.detail ?? apiError?.problem.title ?? error?.message;
  const requestId = apiError?.requestId;

  return (
    <div
      data-slot="error-state"
      role="alert"
      className={cn(
        "flex flex-col items-center justify-center gap-2 rounded-2xl border border-rose/25 bg-rose/5 px-6 py-12 text-center",
        className
      )}
    >
      <div className="flex size-10 items-center justify-center rounded-xl bg-rose/10 text-rose-on-subtle">
        <OctagonX className="size-5" aria-hidden />
      </div>
      <p className="text-sm font-medium text-foreground">
        {title ?? t("error.title")}
      </p>
      <p className="max-w-md text-sm text-muted-foreground">
        {description ?? message ?? t("error.description")}
      </p>
      {apiError ? (
        <p className="text-2xs text-muted-foreground">
          {apiError.code}
          {requestId ? ` · ${t("requestId")}: ${requestId}` : ""}
        </p>
      ) : null}
      {onRetry ? (
        <Button
          type="button"
          variant="outline"
          size="sm"
          className="mt-2"
          onClick={onRetry}
        >
          {retryLabel ?? t("retry")}
        </Button>
      ) : null}
    </div>
  );
}
