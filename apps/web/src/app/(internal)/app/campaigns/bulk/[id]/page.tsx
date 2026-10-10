import { CampaignDetail } from "@/features/campaigns";

export default async function BulkCampaignDetailPage({
  params,
}: {
  params: Promise<{ id: string }>;
}) {
  const { id } = await params;
  return <CampaignDetail campaignId={id} />;
}
