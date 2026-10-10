import {
  CampaignBuilder,
  type BuilderMode,
} from "@/features/campaigns/campaign-builder";
import { PersonalizedCampaignCreate } from "@/features/campaigns/personalized-campaign-create";

export default async function NewCampaignPage({
  searchParams,
}: {
  searchParams: Promise<Record<string, string | string[] | undefined>>;
}) {
  const params = await searchParams;
  const source = typeof params.source === "string" && params.source.trim()
    ? params.source
    : undefined;
  const mode: BuilderMode =
    source && (params.mode === "revision" || params.mode === "clone")
      ? params.mode
      : "create";

  return mode === "create" ? (
    <PersonalizedCampaignCreate />
  ) : (
    <CampaignBuilder mode={mode} sourceCampaignId={source} />
  );
}
