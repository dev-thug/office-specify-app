import { ApprovalDetail } from "@/features/approvals";

export default async function ApprovalDetailPage({
  params,
}: {
  params: Promise<{ id: string }>;
}) {
  const { id } = await params;
  return <ApprovalDetail approvalId={id} />;
}
