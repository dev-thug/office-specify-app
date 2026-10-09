import { ImportDetailPage } from "@/features/imports";

interface Props {
  params: Promise<{ id: string }>;
}

export default async function ImportRoutePage({ params }: Props) {
  const { id } = await params;
  return <ImportDetailPage importId={id} />;
}
