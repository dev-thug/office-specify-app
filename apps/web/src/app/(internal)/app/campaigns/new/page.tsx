import { CampaignBuilder, type BuilderMode } from "@/features/campaigns";

/**
 * §7.5 캠페인 6단계 작성 화면.
 * ?source=<id>&mode=revision|clone — 기존 캠페인의 새 revision 또는 복제.
 */
export default async function NewCampaignPage({
  searchParams,
}: {
  searchParams: Promise<Record<string, string | string[] | undefined>>;
}) {
  const sp = await searchParams;
  const source = typeof sp.source === "string" ? sp.source : undefined;
  const mode: BuilderMode =
    source && (sp.mode === "revision" || sp.mode === "clone")
      ? sp.mode
      : "create";
  return <CampaignBuilder mode={mode} sourceCampaignId={source} />;
}
