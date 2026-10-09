import { ContactDetailScreen } from "@/features/contacts";

export default async function ContactDetailPage({ params }: { params: Promise<{ id: string }> }) {
  const { id } = await params;
  return <ContactDetailScreen contactId={id} />;
}
