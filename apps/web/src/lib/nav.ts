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
  /** Secondary routes remain searchable and can be grouped under a primary route. */
  children?: NavItem[];
  /** Extra exact and localized terms for command-palette discovery. */
  searchTerms?: string[];
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
      {
        key: "companies",
        href: "/app/companies",
        icon: Building2,
        children: [
          { key: "contacts", href: "/app/contacts", icon: Users },
        ],
      },
      {
        key: "campaigns",
        href: "/app/campaigns",
        icon: Megaphone,
        children: [
          { key: "approvals", href: "/app/approvals", icon: ListChecks },
        ],
      },
      { key: "inbox", href: "/app/inbox", icon: Inbox },
      { key: "templates", href: "/app/templates", icon: FileText },
      {
        key: "meetings",
        href: "/app/meetings",
        icon: CalendarDays,
        children: [
          { key: "meetingTypes", href: "/app/meeting-types", icon: CalendarCog },
        ],
      },
      {
        key: "settings",
        href: "/app/settings",
        icon: Settings,
        children: [
          { key: "settingsMembers", href: "/app/settings/members", icon: Users, searchTerms: ["MCP", "API key", "service credential", "agent access", "MCP 자격 증명", "API 키", "서비스 credential"] },
          { key: "settingsMailAccounts", href: "/app/settings/mail-accounts", icon: Mail },
          { key: "settingsCalendars", href: "/app/settings/calendars", icon: CalendarDays },
          { key: "settingsPolicies", href: "/app/settings/policies", icon: ShieldCheck },
          { key: "settingsSecurity", href: "/app/settings/security", icon: BookOpen },
          { key: "agents", href: "/app/agents", icon: Bot },
          { key: "imports", href: "/app/imports", icon: Upload },
          {
            key: "compliance",
            href: "/app/compliance/suppressions",
            icon: ShieldOff,
            children: [
              { key: "complianceReviews", href: "/app/compliance/reviews", icon: ClipboardCheck },
            ],
          },
          {
            key: "operations",
            href: "/app/operations/jobs",
            icon: Wrench,
            children: [
              { key: "operationsIntegrations", href: "/app/operations/integrations", icon: Plug },
            ],
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
  rootKey: string;
  parentKey?: string;
  searchTerms?: string[];
}

export function flattenNav(): FlatNavItem[] {
  function flattenItems(
    items: NavItem[],
    sectionKey: string,
    depth = 0,
    rootKey?: string,
    parentKey?: string
  ): FlatNavItem[] {
    return items.flatMap((item) => {
      const itemRootKey = rootKey ?? item.key;
      return [
        {
          key: item.key,
          href: item.href,
          icon: item.icon,
          depth,
          sectionKey,
          rootKey: itemRootKey,
          ...(parentKey ? { parentKey } : {}),
          ...(item.searchTerms ? { searchTerms: item.searchTerms } : {}),
        },
        ...flattenItems(
          item.children ?? [],
          sectionKey,
          depth + 1,
          itemRootKey,
          item.key
        ),
      ];
    });
  }

  return NAV_SECTIONS.flatMap((section) =>
    flattenItems(section.items, section.key)
  );
}

/** Resolves deep routes to their visible primary navigation destination. */
/** Returns the most specific route context for headers and feature help. */
export function navItemForPath(pathname: string): FlatNavItem | null {
  return (
    flattenNav()
      .filter((item) =>
        item.key === "dashboard"
          ? pathname === item.href
          : pathname === item.href || pathname.startsWith(`${item.href}/`)
      )
      .sort((left, right) => right.href.length - left.href.length)[0] ?? null
  );
}

export function primaryNavItemForPath(pathname: string): FlatNavItem | null {
  const route = navItemForPath(pathname);
  if (!route) return null;
  return flattenNav().find((item) => item.depth === 0 && item.key === route.rootKey) ?? null;
}

/** Highlights only the primary navigation entry for the current route. */
export function isPrimaryNavActive(pathname: string, href: string): boolean {
  return primaryNavItemForPath(pathname)?.href === href;
}
