import { SavedDraftDetailScreen } from "@/features/sending";

interface SavedDraftDetailPageProps {
  params: Promise<{ draftId: string }>;
}

export default async function SavedDraftDetailPage({
  params,
}: SavedDraftDetailPageProps) {
  const { draftId } = await params;
  return <SavedDraftDetailScreen draftId={draftId} />;
}
