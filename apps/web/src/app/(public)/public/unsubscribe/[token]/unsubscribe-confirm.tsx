"use client";

import { useMutation, useQuery } from "@tanstack/react-query";
import { CircleCheckIcon, Link2OffIcon } from "lucide-react";
import { useTranslations } from "next-intl";
import { useRef, useState } from "react";

import { ErrorState } from "@/components/patterns/error-state";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import {
  confirmUnsubscribe,
  getUnsubscribePreview,
  type UnsubscribeResult,
} from "@/lib/api/compliance";
import { idempotencyKey } from "@/lib/api/client";
import { ApiError } from "@/lib/api/problem";

/** 토큰의 현재 상태 — GET 미리보기 status 또는 POST 결과로 정한다. */
type ViewState =
  | { kind: "confirm" }
  | { kind: "done"; result?: UnsubscribeResult }
  | { kind: "invalid" };

/**
 * 공개 수신 거부 확인 (§7.6 "수신 거부 확인").
 *
 * - GET /public/v1/unsubscribe/{token}는 확인 데이터만 반환한다 — 부작용 없음.
 * - 버튼의 POST만 실제 차단을 실행한다. 반복 클릭·새로고침 재제출은 같은 성공.
 * - 발신 조직·차단 범위만 보여주고 주소 전체·CRM ID는 표시하지 않는다
 *   (마스킹된 주소만). 로그인·이메일 재입력·설문을 요구하지 않는다.
 * - 추적 픽셀·외부 스크립트는 두지 않는다.
 */
export function UnsubscribeConfirm({ token }: { token: string }) {
  const t = useTranslations("compliance.unsubscribe");

  // POST는 토큰 자체가 멱등하지만(B.4.4 반복 요청 = 같은 성공), 네트워크
  // 재시도가 같은 논리 요청임을 명시하기 위해 키를 마운트 시 한 번만 발급한다.
  const keyRef = useRef<string>(idempotencyKey());
  const [state, setState] = useState<ViewState>({ kind: "confirm" });

  const preview = useQuery({
    queryKey: ["compliance", "public-unsubscribe", token],
    queryFn: () => getUnsubscribePreview(token),
    // 이미 처리된 링크 정보는 바뀌지 않으므로 캐시를 조금 길게 둔다.
    staleTime: 60_000,
    retry: (failureCount, error) => {
      // 404/410은 존재 은닉용 일반화 응답 — 재시도해도 같다.
      if (error instanceof ApiError && error.status < 500) return false;
      return failureCount < 2;
    },
  });

  const submit = useMutation({
    mutationFn: () =>
      confirmUnsubscribe(token, { idempotencyKey: keyRef.current }),
    onSuccess: (res) => setState({ kind: "done", result: res.data }),
    onError: (e) => {
      // 토큰 만료·폐기·알 수 없는 토큰은 동일한 "사용 불가" 상태로 보인다 —
      // 주소 존재 여부를 오류 차이로 드러내지 않는다 (B.4.4).
      if (
        e instanceof ApiError &&
        (e.status === 404 || e.status === 410 || e.code === "NOT_FOUND")
      ) {
        setState({ kind: "invalid" });
      }
    },
  });

  // GET 응답이 상태를 알려오면(이미 처리/만료) 그것을 우선한다.
  const previewStatus = preview.data?.data.status;
  const effectiveState: ViewState =
    state.kind === "confirm" &&
    (previewStatus === "unsubscribed" || previewStatus === "already_unsubscribed")
      ? { kind: "done" }
      : state.kind === "confirm" &&
          (previewStatus === "expired" ||
            previewStatus === "revoked" ||
            previewStatus === "invalid")
        ? { kind: "invalid" }
        : state;

  if (preview.isPending) {
    return (
      <div
        className="flex flex-col gap-5"
        role="status"
        aria-label={t("checking")}
      >
        <Skeleton className="h-8 w-40" />
        <Skeleton className="h-4 w-full max-w-sm" />
        <Skeleton className="h-24 w-full rounded-2xl" />
        <Skeleton className="h-10 w-full rounded-full" />
      </div>
    );
  }

  // 알 수 없는 토큰(404)과 일시 오류를 구분한다 — 404는 재시도 의미 없음.
  if (preview.isError) {
    const e = preview.error;
    const invalid =
      e instanceof ApiError &&
      (e.status === 404 || e.status === 410 || e.code === "NOT_FOUND");
    if (invalid) {
      return <InvalidLink t={t} />;
    }
    return (
      <ErrorState
        title={t("errorTitle")}
        description={t("loadError")}
        error={e}
        onRetry={() => preview.refetch()}
        retryLabel={t("retry")}
      />
    );
  }

  if (effectiveState.kind === "invalid") {
    return <InvalidLink t={t} />;
  }

  if (effectiveState.kind === "done") {
    const message = effectiveState.result?.message;
    return (
      <div className="flex flex-col gap-5">
        <div className="flex items-start gap-3">
          <CircleCheckIcon
            className="mt-0.5 size-6 shrink-0 text-success-on-subtle"
            aria-hidden
          />
          <div>
            <h1 className="text-2xl font-semibold">{t("doneTitle")}</h1>
            <p className="mt-1 text-sm text-muted-foreground">
              {message ?? t("doneDescription")}
            </p>
          </div>
        </div>
        <p className="rounded-2xl border border-border bg-card px-4 py-3 text-sm text-muted-foreground">
          {t("doneScope", { scope: t("scope.workspace_marketing") })}
        </p>
      </div>
    );
  }

  const previewData = preview.data?.data;

  return (
    <div className="flex flex-col gap-5">
      <div>
        <h1 className="text-2xl font-semibold">{t("title")}</h1>
        <p className="mt-1 text-sm text-muted-foreground">
          {t("description")}
        </p>
      </div>

      <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5 rounded-2xl border border-border bg-card px-4 py-3 text-sm">
        <dt className="text-muted-foreground">{t("organization")}</dt>
        <dd className="font-medium">
          {previewData?.organization_name ?? t("organizationFallback")}
        </dd>
        <dt className="text-muted-foreground">{t("scopeLabel")}</dt>
        <dd className="font-medium">
          {scopeLabel(t, previewData?.scope)}
        </dd>
        {previewData?.masked_address ? (
          <>
            <dt className="text-muted-foreground">{t("address")}</dt>
            <dd className="font-medium">{previewData.masked_address}</dd>
          </>
        ) : null}
      </dl>

      {submit.isError ? (
        <Alert variant="destructive">
          <AlertTitle>{t("errorTitle")}</AlertTitle>
          <AlertDescription>
            {submit.error instanceof ApiError &&
            submit.error.problem.retryable === false
              ? (submit.error.problem.detail ?? t("submitError"))
              : t("submitError")}
          </AlertDescription>
        </Alert>
      ) : null}

      <Button
        type="button"
        variant="destructive"
        className="w-full"
        disabled={submit.isPending}
        onClick={() => submit.mutate()}
      >
        {submit.isPending ? t("confirming") : t("confirm")}
      </Button>
      <p className="text-xs text-muted-foreground">{t("footnote")}</p>
    </div>
  );
}

/** 범위 문자열 → 지역화 라벨. 알 수 없는 값은 원문을 그대로 보인다. */
function scopeLabel(
  t: ReturnType<typeof useTranslations>,
  scope: string | undefined
) {
  const key = `scope.${scope ?? "workspace_marketing"}`;
  return t.has(key) ? t(key) : (scope ?? t("scope.workspace_marketing"));
}

function InvalidLink({ t }: { t: ReturnType<typeof useTranslations> }) {
  return (
    <div className="flex flex-col gap-5">
      <div className="flex items-start gap-3">
        <Link2OffIcon
          className="mt-0.5 size-6 shrink-0 text-muted-foreground"
          aria-hidden
        />
        <div>
          <h1 className="text-2xl font-semibold">{t("invalidTitle")}</h1>
          <p className="mt-1 text-sm text-muted-foreground">
            {t("invalidDescription")}
          </p>
        </div>
      </div>
    </div>
  );
}
