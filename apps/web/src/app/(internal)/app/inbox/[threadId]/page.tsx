import { ThreadDetailScreen } from "@/features/inbox";

interface ThreadDetailPageProps {
  params: Promise<{ threadId: string }>;
}

export default async function ThreadDetailPage({
  params,
}: ThreadDetailPageProps) {
  const { threadId } = await params;
  return <ThreadDetailScreen threadId={threadId} />;
}
