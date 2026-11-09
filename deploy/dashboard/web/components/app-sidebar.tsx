"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import {
  ActivityIcon,
  BadgeCheckIcon,
  BoxesIcon,
  ExternalLinkIcon,
  FingerprintIcon,
  GlobeIcon,
  LayoutDashboardIcon,
  ServerIcon,
  WalletIcon,
} from "lucide-react";
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarHeader,
  SidebarMenu,
  SidebarMenuBadge,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarRail,
} from "@/components/ui/sidebar";
import { useDevnet } from "@/lib/api";
import { StatusDot } from "@/components/status";

export function AppSidebar() {
  const path = usePathname();
  const { nodes, cells, humans, jobs, settlement, containers, dozzle, error, block } = useDevnet();
  const running = Object.values(jobs).filter((j) => j.status === "running").length;
  const up = nodes.filter((n) => n.up !== false && n.active).length;
  const nav = [
    { group: "Network", items: [
      { href: "/", label: "Overview", icon: LayoutDashboardIcon },
      { href: "/network/", label: "Latency map", icon: GlobeIcon, badge: cells.length || undefined },
      { href: "/nodes/", label: "Nodes", icon: ServerIcon, badge: nodes.length ? `${up}/${nodes.length}` : undefined },
    ] },
    { group: "Humans", items: [
      { href: "/humans/", label: "Clients & attest", icon: ActivityIcon, badge: humans.length || undefined },
      { href: "/world-id/", label: "World ID", icon: FingerprintIcon },
    ] },
    { group: "Money & ops", items: [
      { href: "/settlement/", label: "Settlement", icon: WalletIcon, badge: settlement?.periods.length || undefined },
      { href: "/ops/", label: "Operations", icon: BoxesIcon, badge: running ? `${running} running` : containers.length || undefined },
    ] },
  ];
  const active = (href: string) => (href === "/" ? path === "/" : path.startsWith(href.replace(/\/$/, "")));
  return (
    <Sidebar collapsible="icon" variant="inset">
      <SidebarHeader>
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton size="lg" asChild>
              <Link href="/">
                <div className="flex aspect-square size-8 items-center justify-center rounded-lg bg-gradient-to-br from-primary to-chart-2 text-primary-foreground shadow-[0_0_20px] shadow-primary/30">
                  <BadgeCheckIcon className="size-4" />
                </div>
                <div className="grid flex-1 text-left leading-tight">
                  <span className="truncate font-semibold">Dedan</span>
                  <span className="truncate text-xs text-muted-foreground">decentralized CDN · devnet</span>
                </div>
              </Link>
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarHeader>
      <SidebarContent>
        {nav.map((g) => (
          <SidebarGroup key={g.group}>
            <SidebarGroupLabel>{g.group}</SidebarGroupLabel>
            <SidebarGroupContent>
              <SidebarMenu>
                {g.items.map((it) => (
                  <SidebarMenuItem key={it.href}>
                    <SidebarMenuButton asChild isActive={active(it.href)} tooltip={it.label}>
                      <Link href={it.href}>
                        <it.icon />
                        <span>{it.label}</span>
                      </Link>
                    </SidebarMenuButton>
                    {it.badge !== undefined && <SidebarMenuBadge className="font-mono text-[10px]">{it.badge}</SidebarMenuBadge>}
                  </SidebarMenuItem>
                ))}
              </SidebarMenu>
            </SidebarGroupContent>
          </SidebarGroup>
        ))}
        <SidebarGroup className="mt-auto">
          <SidebarGroupContent>
            <SidebarMenu>
              <SidebarMenuItem>
                <SidebarMenuButton asChild tooltip="Logs & terminals (Dozzle)">
                  <a href={dozzle} target="_blank" rel="noreferrer">
                    <ExternalLinkIcon />
                    <span>Logs & terminals</span>
                  </a>
                </SidebarMenuButton>
              </SidebarMenuItem>
              <SidebarMenuItem>
                <SidebarMenuButton asChild tooltip="Raw API state">
                  <a href="/api/state" target="_blank" rel="noreferrer">
                    <ExternalLinkIcon />
                    <span>API</span>
                  </a>
                </SidebarMenuButton>
              </SidebarMenuItem>
            </SidebarMenu>
          </SidebarGroupContent>
        </SidebarGroup>
      </SidebarContent>
      <SidebarFooter>
        <div className="flex items-center gap-2 rounded-lg border bg-sidebar-accent/40 px-3 py-2 text-xs group-data-[collapsible=icon]:hidden">
          <StatusDot tone={error ? "bad" : block ? "ok" : "warn"} pulse={!error} />
          <span className="text-muted-foreground">{error ? "dashboard API unreachable" : block ? "chain" : "syncing"}</span>
          {block !== undefined && !error && <span className="ml-auto font-mono">#{block}</span>}
        </div>
      </SidebarFooter>
      <SidebarRail />
    </Sidebar>
  );
}
