import { useTranslations } from "next-intl";
import { InfoIcon } from "lucide-react";

import { EmptyState } from "@/components/patterns/empty-state";
import { PageHeader } from "@/components/patterns/page-header";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";

export default function CompanyDetailPage() {
  const t = useTranslations("screens.companyDetail");
  const ts = useTranslations("screens");
  const tabs = t("tabs").split("|");
  return (
    <div className="mx-auto flex w-full max-w-6xl flex-col gap-5 px-6 py-6">
      <PageHeader title={t("title")} description={t("description")} />
      <Alert variant="info">
        <InfoIcon />
        <AlertTitle>{ts("placeholderTitle")}</AlertTitle>
        <AlertDescription>{ts("placeholderDescription")}</AlertDescription>
      </Alert>
      <Tabs defaultValue={tabs[0]}>
        <TabsList className="flex-wrap">
          {tabs.map((tab) => (
            <TabsTrigger key={tab} value={tab}>
              {tab}
            </TabsTrigger>
          ))}
        </TabsList>
        {tabs.map((tab) => (
          <TabsContent key={tab} value={tab} className="pt-2">
            <EmptyState
              title={t("emptyTitle")}
              description={t("emptyDescription")}
            />
          </TabsContent>
        ))}
      </Tabs>
    </div>
  );
}
