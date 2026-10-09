"use client";

import { useTranslations } from "next-intl";
import { useState, type ReactNode } from "react";
import { toast } from "sonner";
import { StarIcon } from "lucide-react";

import { ConfirmIrreversible } from "@/components/patterns/confirm-irreversible";
import { ConflictDiff, type ConflictChoice } from "@/components/patterns/conflict-diff";
import { DataTable, NO_SELECTION, type ColumnDef, type RowSelection } from "@/components/patterns/data-table";
import { EmptyState } from "@/components/patterns/empty-state";
import { ErrorState } from "@/components/patterns/error-state";
import { StatusBadge } from "@/components/patterns/status-badge";
import {
  Accordion,
  AccordionContent,
  AccordionItem,
  AccordionTrigger,
} from "@/components/ui/accordion";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Avatar, AvatarFallback } from "@/components/ui/avatar";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Separator } from "@/components/ui/separator";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
  SheetTrigger,
} from "@/components/ui/sheet";
import { Skeleton } from "@/components/ui/skeleton";
import { Spinner } from "@/components/ui/spinner";
import { Switch } from "@/components/ui/switch";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Textarea } from "@/components/ui/textarea";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { ApiError } from "@/lib/api/problem";

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="flex flex-col gap-3">
      <h3 className="text-sm font-semibold text-muted-foreground">{title}</h3>
      <div className="flex flex-wrap items-start gap-2">{children}</div>
    </section>
  );
}

const SAMPLE_STATUSES = [
  "active", "pending", "awaiting_approval", "confirming", "unknown",
  "delivered", "bounced", "suppressed", "running", "dead", "canceled",
];

interface Row {
  id: string;
  name: string;
  stage: string;
  status: string;
}

const SAMPLE_ROWS: Row[] = [
  { id: "r1", name: "Acme Corp", stage: "Engaged", status: "active" },
  { id: "r2", name: "Globex", stage: "Qualified", status: "pending" },
  { id: "r3", name: "Initech", stage: "Prospect", status: "unknown" },
];

export function DesignGallery() {
  const t = useTranslations("design");
  const [selection, setSelection] = useState<RowSelection>(NO_SELECTION);
  const [conflictChoices, setConflictChoices] = useState<Record<string, ConflictChoice>>({
    title: "mine",
    owner: "server",
  });
  const [confirmOpen, setConfirmOpen] = useState(false);

  const columns: ColumnDef<Row>[] = [
    { key: "name", header: "Name", cell: (r) => <span className="font-medium">{r.name}</span> },
    { key: "stage", header: "Stage", cell: (r) => r.stage },
    { key: "status", header: "Status", cell: (r) => <StatusBadge status={r.status} /> },
  ];

  return (
    <div className="flex flex-col gap-8 text-foreground">
      <Section title={t("buttons")}>
        {(["default", "secondary", "outline", "ghost", "link", "destructive"] as const).map(
          (v) => (
            <Button key={v} variant={v} size="sm">
              {v}
            </Button>
          )
        )}
        <Button size="sm" disabled>
          disabled
        </Button>
        <Button size="icon-sm" aria-label="favorite">
          <StarIcon />
        </Button>
      </Section>

      <Section title={t("badges")}>
        {(["default", "secondary", "outline", "success", "info", "amber", "rose", "teal", "success-subtle", "info-subtle", "amber-subtle", "rose-subtle", "teal-subtle"] as const).map(
          (v) => (
            <Badge key={v} variant={v}>
              {v}
            </Badge>
          )
        )}
      </Section>

      <Section title={t("statusBadges")}>
        {SAMPLE_STATUSES.map((s) => (
          <StatusBadge key={s} status={s} />
        ))}
      </Section>

      <Section title={t("forms")}>
        <div className="grid w-full max-w-md gap-3">
          <div className="grid gap-1.5">
            <Label htmlFor="dg-input">Input</Label>
            <Input id="dg-input" placeholder="placeholder" />
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="dg-textarea">Textarea</Label>
            <Textarea id="dg-textarea" placeholder="placeholder" />
          </div>
          <div className="flex items-center gap-4">
            <Switch id="dg-switch" />
            <Label htmlFor="dg-switch">Switch</Label>
            <Checkbox id="dg-check" />
            <Label htmlFor="dg-check">Checkbox</Label>
          </div>
          <Select>
            <SelectTrigger className="w-56">
              <SelectValue placeholder="Select…" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="a">Option A</SelectItem>
              <SelectItem value="b">Option B</SelectItem>
            </SelectContent>
          </Select>
        </div>
      </Section>

      <Section title={t("surfaces")}>
        <Card className="w-64">
          <CardHeader>
            <CardTitle>Card</CardTitle>
            <CardDescription>default surface</CardDescription>
          </CardHeader>
          <CardContent className="text-sm text-muted-foreground">
            rounded-2xl · border · bg-card
          </CardContent>
        </Card>
        <Card variant="glass" className="w-64">
          <CardHeader>
            <CardTitle>Glass</CardTitle>
            <CardDescription>overlay surface</CardDescription>
          </CardHeader>
          <CardContent>
            <Skeleton className="h-4 w-3/4" />
          </CardContent>
        </Card>
        <Avatar className="size-9">
          <AvatarFallback>OS</AvatarFallback>
        </Avatar>
      </Section>

      <Section title={t("feedback")}>
        <Alert className="max-w-md" variant="info">
          <AlertTitle>Info</AlertTitle>
          <AlertDescription>informational banner</AlertDescription>
        </Alert>
        <Alert className="max-w-md" variant="warning">
          <AlertTitle>Warning</AlertTitle>
          <AlertDescription>amber banner</AlertDescription>
        </Alert>
        <Spinner />
        <Button
          variant="outline"
          size="sm"
          onClick={() => toast.success("Saved")}
        >
          {t("showToast")}
        </Button>
      </Section>

      <Section title={t("navigation")}>
        <Tabs defaultValue="a">
          <TabsList>
            <TabsTrigger value="a">Tab A</TabsTrigger>
            <TabsTrigger value="b">Tab B</TabsTrigger>
          </TabsList>
          <TabsContent value="a" className="text-sm text-muted-foreground">
            Tab A content
          </TabsContent>
          <TabsContent value="b" className="text-sm text-muted-foreground">
            Tab B content
          </TabsContent>
        </Tabs>
        <Accordion type="single" collapsible className="w-64">
          <AccordionItem value="i1">
            <AccordionTrigger>Accordion</AccordionTrigger>
            <AccordionContent>Collapsible body</AccordionContent>
          </AccordionItem>
        </Accordion>
      </Section>

      <Section title={t("overlays")}>
        <Dialog>
          <DialogTrigger asChild>
            <Button variant="outline" size="sm">{t("openDialog")}</Button>
          </DialogTrigger>
          <DialogContent>
            <DialogHeader>
              <DialogTitle>Dialog</DialogTitle>
              <DialogDescription>rounded-3xl modal surface</DialogDescription>
            </DialogHeader>
          </DialogContent>
        </Dialog>
        <Sheet>
          <SheetTrigger asChild>
            <Button variant="outline" size="sm">{t("openSheet")}</Button>
          </SheetTrigger>
          <SheetContent>
            <SheetHeader>
              <SheetTitle>Sheet</SheetTitle>
              <SheetDescription>side panel</SheetDescription>
            </SheetHeader>
          </SheetContent>
        </Sheet>
        <Popover>
          <PopoverTrigger asChild>
            <Button variant="outline" size="sm">Popover</Button>
          </PopoverTrigger>
          <PopoverContent className="w-56 text-sm">popover body</PopoverContent>
        </Popover>
        <Tooltip>
          <TooltipTrigger asChild>
            <Button variant="outline" size="sm">Tooltip</Button>
          </TooltipTrigger>
          <TooltipContent>tooltip text</TooltipContent>
        </Tooltip>
        <Separator orientation="vertical" className="h-6" />
      </Section>

      <section className="flex flex-col gap-3">
        <h3 className="text-sm font-semibold text-muted-foreground">{t("dataTable")}</h3>
        <DataTable
          columns={columns}
          rows={SAMPLE_ROWS}
          rowKey={(r) => r.id}
          selectable
          selection={selection}
          onSelectionChange={setSelection}
          filteredTotal={42}
          page={{ next_cursor: "next", has_more: true }}
          onLoadMore={() => undefined}
        />
      </section>

      <section className="flex flex-col gap-3">
        <h3 className="text-sm font-semibold text-muted-foreground">{t("conflictDiff")}</h3>
        <ConflictDiff
          fields={[
            { key: "title", label: "Title", base: "Old title", server: "Server title", mine: "My title" },
            { key: "owner", label: "Owner", base: "Kim", server: "Lee", mine: "Kim" },
          ]}
          choices={conflictChoices}
          onChoiceChange={(key, c) => setConflictChoices((p) => ({ ...p, [key]: c }))}
        />
      </section>

      <Section title={t("confirmIrreversible")}>
        <Button variant="destructive" size="sm" onClick={() => setConfirmOpen(true)}>
          {t("sampleAction")}
        </Button>
        <ConfirmIrreversible
          open={confirmOpen}
          onOpenChange={setConfirmOpen}
          title="Launch campaign"
          recipientsCount={128}
          sender="sales@example.com"
          purpose="Cold outreach"
          contentPreview={{ subject: "안녕하세요", body: "본문 미리보기…" }}
          confirmText="128"
          onConfirm={() => setConfirmOpen(false)}
        />
      </Section>

      <section className="flex flex-col gap-3">
        <h3 className="text-sm font-semibold text-muted-foreground">{t("emptyError")}</h3>
        <div className="grid gap-3 sm:grid-cols-2">
          <EmptyState title="No rows" description="nothing matched" />
          <ErrorState
            error={
              new ApiError({
                type: "about:blank",
                title: "Service unavailable",
                status: 503,
                code: "upstream_unavailable",
                requestId: "req-demo-1",
                violations: [],
              })
            }
            onRetry={() => undefined}
          />
        </div>
      </section>
    </div>
  );
}
