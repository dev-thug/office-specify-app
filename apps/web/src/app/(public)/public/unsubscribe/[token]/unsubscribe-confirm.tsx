"use client";

import { useMutation, useQuery } from "@tanstack/react-query";
import { CheckIcon, Link2OffIcon, MailIcon, MailXIcon } from "lucide-react";
import { useTranslations } from "next-intl";
import { type ReactNode, useRef, useState } from "react";

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
import { cn } from "@/lib/utils";

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
 * - 발신 조직과 마스킹된 주소만 보여주고 주소 전체·CRM ID는 표시하지 않는다. 로그인·이메일 재입력·설문을 요구하지 않는다.
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
        className="mx-auto flex w-full max-w-sm flex-1 flex-col items-center gap-3 pt-10 sm:max-w-md sm:justify-center sm:pt-0 sm:pb-16"
        role="status"
        aria-label={t("checking")}
      >
        <Skeleton className="size-14 rounded-full" />
        <Skeleton className="mt-3 h-8 w-52" />
        <Skeleton className="h-4 w-64" />
        <Skeleton className="mt-6 h-12 w-full rounded-full" />
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
      <div className="mx-auto flex w-full max-w-sm flex-1 flex-col pt-10 sm:max-w-md sm:justify-center sm:pt-0 sm:pb-16">
        <ErrorState
          title={t("errorTitle")}
          description={t("loadError")}
          error={e}
          onRetry={() => preview.refetch()}
          retryLabel={t("retry")}
        />
      </div>
    );
  }

  if (effectiveState.kind === "invalid") {
    return <InvalidLink t={t} />;
  }

  if (effectiveState.kind === "done") {
    return (
      <StatusScreen
        icon={
          <IconCircle className="bg-success/10 text-success-on-subtle">
            <CheckIcon className="size-7" strokeWidth={2.5} aria-hidden />
          </IconCircle>
        }
        title={t("doneTitle")}
        description={t("doneDescription")}
      />
    );
  }

  const previewData = preview.data?.data;
  const organization = previewData?.organization_name;

  return (
    <StatusScreen
      icon={
        <IconCircle className="bg-muted text-muted-foreground">
          <MailXIcon className="size-6" aria-hidden />
        </IconCircle>
      }
      title={
        organization
          ? t.rich("titleWithOrganization", {
              organization,
              org: (chunks) => (
                <span className="text-brand-on-subtle">{chunks}</span>
              ),
            })
          : t("title")
      }
      description={t("description")}
      actions={
        <>
          {submit.isError ? (
            <Alert variant="destructive" className="text-left">
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
            size="xl"
            className="w-full"
            disabled={submit.isPending}
            onClick={() => submit.mutate()}
          >
            {submit.isPending ? t("confirming") : t("confirm")}
          </Button>
          <p className="text-ui break-keep text-muted-foreground">
            {t("footnote")}
          </p>
        </>
      }
    >
      {previewData?.masked_address ? (
        <p className="inline-flex items-center gap-1.5 rounded-full bg-muted px-3 py-1 text-ui text-muted-foreground">
          <MailIcon className="size-3.5" aria-hidden />
          <span className="sr-only">{t("address")}</span>
          <span>{previewData.masked_address}</span>
        </p>
      ) : null}
    </StatusScreen>
  );
}

function IconCircle({
  className,
  children,
}: {
  className: string;
  children: ReactNode;
}) {
  return (
    <span
      className={cn(
        "flex size-14 items-center justify-center rounded-full sm:size-16",
        className
      )}
    >
      {children}
    </span>
  );
}

/**
 * 공개 수신 거부의 모든 상태가 공유하는 가운데 정렬 화면 골격.
 * 모바일은 actions를 화면 하단에 고정하고, 넓은 화면은 콘텐츠와 한 묶음으로
 * 세로 가운데에 둔다.
 */
function StatusScreen({
  icon,
  title,
  description,
  children,
  actions,
}: {
  icon: ReactNode;
  title: ReactNode;
  description: string;
  children?: ReactNode;
  actions?: ReactNode;
}) {
  return (
    <div className="mx-auto flex w-full max-w-sm flex-1 flex-col sm:max-w-md sm:justify-center sm:pb-16">
      <div className="flex flex-col items-center pt-10 text-center sm:pt-0">
        {icon}
        <h1 className="mt-6 text-2xl leading-snug font-semibold break-keep whitespace-pre-line sm:mt-8 sm:text-3xl">
          {title}
        </h1>
        <p className="mt-3 text-sm leading-relaxed break-keep whitespace-pre-line text-muted-foreground sm:mt-4 sm:text-base">
          {description}
        </p>
        {children ? (
          <div className="mt-6 flex flex-col items-center gap-3 sm:mt-8">
            {children}
          </div>
        ) : null}
      </div>
      {actions ? (
        <div className="sticky bottom-0 mt-auto flex flex-col gap-3 bg-background pt-10 pb-[max(0.5rem,env(safe-area-inset-bottom))] text-center sm:static sm:mt-10 sm:p-0">
          {actions}
        </div>
      ) : null}
    </div>
  );
}

function InvalidLink({ t }: { t: ReturnType<typeof useTranslations> }) {
  return (
    <StatusScreen
      icon={
        <IconCircle className="bg-muted text-muted-foreground">
          <Link2OffIcon className="size-6" aria-hidden />
        </IconCircle>
      }
      title={t("invalidTitle")}
      description={t("invalidDescription")}
    />
  );
}
