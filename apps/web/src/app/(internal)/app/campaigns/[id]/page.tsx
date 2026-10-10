import { PersonalizedCampaignDetail } from "@/features/campaigns/personalized-campaign-detail";

export default async function CampaignDetailPage({
  params,
}: {
  params: Promise<{ id: string }>;
}) {
  const { id } = await params;
  return <PersonalizedCampaignDetail campaignId={id} />;
}
