import { CompanyDetailScreen } from "@/features/companies";

export default async function CompanyDetailPage({ params }: { params: Promise<{ id: string }> }) {
  const { id } = await params;
  return <CompanyDetailScreen companyId={id} />;
}
