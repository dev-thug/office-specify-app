import { useTranslations } from "next-intl";
import type { ReactNode } from "react";

import { EmptyState } from "@/components/patterns/empty-state";
import { PageHeader } from "@/components/patterns/page-header";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { InfoIcon } from "lucide-react";

interface ScreenStubProps {
  /** messages > screens.<key>.* */
  screen: string;
  /** 우측 액션 슬롯 (예: "새로 만들기" 버튼 자리). */
  actions?: ReactNode;
  /** EmptyState 위에 표시할 추가 구조 (탭·단계 등). */
  children?: ReactNode;
}

/**
 * 라우트 스텁 — §7.3 화면별 설계 카피로 PageHeader + EmptyState를 그린다.
 * 실 데이터 연결은 후속 태스크에서 한다.
 */
export function ScreenStub({ screen, actions, children }: ScreenStubProps) {
  const t = useTranslations("screens");
  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-6 py-6">
      <PageHeader
        title={t(`${screen}.title`)}
        description={t(`${screen}.description`)}
        actions={actions}
      />
      <Alert variant="info">
        <InfoIcon />
        <AlertTitle>{t("placeholderTitle")}</AlertTitle>
        <AlertDescription>{t("placeholderDescription")}</AlertDescription>
      </Alert>
      {children}
      <EmptyState
        title={t(`${screen}.emptyTitle`)}
        description={t(`${screen}.emptyDescription`)}
      />
    </div>
  );
}
