import { CampaignBuilder, type BuilderMode } from "@/features/campaigns";

export default async function NewBulkCampaignPage({
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
