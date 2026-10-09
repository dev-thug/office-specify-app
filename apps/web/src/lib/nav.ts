import {
  BookOpen,
  Building2,
  CalendarDays,
  CalendarCog,
  Inbox,
  LayoutDashboard,
  ListChecks,
  Bot,
  Mail,
  FileText,
  Megaphone,
  ShieldCheck,
  Settings,
  Upload,
  Users,
  Wrench,
  Plug,
  ShieldOff,
  ClipboardCheck,
  type LucideIcon,
} from "lucide-react";

export interface NavItem {
  /** messages > nav.<key> */
  key: string;
  href: string;
  icon: LucideIcon;
  /** 하위 경로 — nav 행은 들여쓰기로 표시한다 */
  children?: NavItem[];
}

export interface NavSection {
  /** messages > nav.sections.<key> */
  key: string;
  items: NavItem[];
}

/** §7.1 정보 구조 — 13개 영역 전체를 사이드바에 노출한다. */
export const NAV_SECTIONS: NavSection[] = [
  {
    key: "main",
    items: [
      { key: "dashboard", href: "/app/dashboard", icon: LayoutDashboard },
    ],
  },
  {
    key: "crm",
    items: [
      { key: "companies", href: "/app/companies", icon: Building2 },
      { key: "contacts", href: "/app/contacts", icon: Users },
    ],
  },
  {
    key: "communication",
    items: [
      { key: "inbox", href: "/app/inbox", icon: Inbox },
      { key: "templates", href: "/app/templates", icon: FileText },
      { key: "campaigns", href: "/app/campaigns", icon: Megaphone },
      { key: "approvals", href: "/app/approvals", icon: ListChecks },
    ],
  },
  {
    key: "meetings",
    items: [
      {
        key: "meetings",
        href: "/app/meetings",
        icon: CalendarDays,
        children: [
          {
            key: "meetingTypes",
            href: "/app/meeting-types",
            icon: CalendarCog,
          },
        ],
      },
    ],
  },
  {
    key: "automation",
    items: [
      { key: "agents", href: "/app/agents", icon: Bot },
      { key: "imports", href: "/app/imports", icon: Upload },
    ],
  },
  {
    key: "admin",
    items: [
      {
        key: "compliance",
        href: "/app/compliance/suppressions",
        icon: ShieldOff,
        children: [
          {
            key: "complianceReviews",
            href: "/app/compliance/reviews",
            icon: ClipboardCheck,
          },
        ],
      },
      {
        key: "operations",
        href: "/app/operations/jobs",
        icon: Wrench,
        children: [
          {
            key: "operationsIntegrations",
            href: "/app/operations/integrations",
            icon: Plug,
          },
        ],
      },
      {
        key: "settings",
        href: "/app/settings/members",
        icon: Settings,
        children: [
          {
            key: "settingsMailAccounts",
            href: "/app/settings/mail-accounts",
            icon: Mail,
          },
          {
            key: "settingsCalendars",
            href: "/app/settings/calendars",
            icon: CalendarDays,
          },
          {
            key: "settingsPolicies",
            href: "/app/settings/policies",
            icon: ShieldCheck,
          },
          {
            key: "settingsSecurity",
            href: "/app/settings/security",
            icon: BookOpen,
          },
        ],
      },
    ],
  },
];

export interface FlatNavItem {
  key: string;
  href: string;
  icon: LucideIcon;
  depth: number;
  sectionKey: string;
}

export function flattenNav(): FlatNavItem[] {
  return NAV_SECTIONS.flatMap((section) =>
    section.items.flatMap((item) => [
      { key: item.key, href: item.href, icon: item.icon, depth: 0, sectionKey: section.key },
      ...(item.children ?? []).map((child) => ({
        key: child.key,
        href: child.href,
        icon: child.icon,
        depth: 1,
        sectionKey: section.key,
      })),
    ])
  );
}
