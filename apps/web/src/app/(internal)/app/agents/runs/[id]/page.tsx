import { AgentRunDetailPage } from "@/features/agents";

interface Props {
  params: Promise<{ id: string }>;
}

export default async function AgentRunPage({ params }: Props) {
  const { id } = await params;
  return <AgentRunDetailPage runId={id} />;
}
