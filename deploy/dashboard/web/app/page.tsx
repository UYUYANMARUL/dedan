"use client";

import Link from "next/link";
import { ArrowRightIcon, GlobeIcon, HexagonIcon, ServerIcon, UsersIcon, WalletIcon } from "lucide-react";
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Progress } from "@/components/ui/progress";
import { PageHeader } from "@/components/page-header";
import { StatCard } from "@/components/stat-card";
import { MapExplorer } from "@/components/map-explorer";
import { RecentJobs } from "@/components/recent-jobs";
import { Pill, StatusDot } from "@/components/status";
import { fmtUsdc, useDevnet } from "@/lib/api";

export default function Overview() {
  const { nodes, cells, ranks, humans, attestations, reveals, settlement, sets, block } = useDevnet();
  const up = nodes.filter((n) => n.active && n.up !== false).length;
  const voted = cells.filter((c) => c.nodes.length).length;
  const closest: Record<string, number> = {};
  for (const r of ranks) if (r.rings === 0 && r.nodes.length) closest[r.nodes[0][0]] = (closest[r.nodes[0][0]] ?? 0) + 1;
  const inSet = new Set(Object.values(sets).flat());
  const last = settlement?.periods.at(-1);
  return (
    <>
      <PageHeader
        title="Network overview"
        description="Nodes are <label>.dedan.eth names on ENS. Humans verified with World ID vote round trips per cell, and the medians on chain steer every client to its closest node."
      >
        {block !== undefined && <Pill tone="info">block #{block}</Pill>}
        <Pill tone={Object.keys(sets).length ? "ok" : "none"}>{Object.keys(sets).length} account slot set{Object.keys(sets).length === 1 ? "" : "s"}</Pill>
      </PageHeader>

      <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-4">
        <StatCard label="Nodes online" icon={ServerIcon} value={`${up}/${nodes.length}`} hint="active on chain with a recent heartbeat">
          <Progress value={nodes.length ? (up / nodes.length) * 100 : 0} className="h-1.5" />
        </StatCard>
        <StatCard label="Cells" icon={HexagonIcon} accent="from-chart-2/15" value={cells.length} hint={`${voted} with medians on chain · resolution 2 H3`} />
        <StatCard
          label="Attestations"
          icon={UsersIcon}
          accent="from-chart-3/15"
          value={attestations.toLocaleString()}
          hint={`${humans.length} humans with devices · ${reveals} key reveals`}
        />
        <StatCard
          label="Settlement"
          icon={WalletIcon}
          accent="from-chart-4/15"
          value={last ? `#${last.period}` : "idle"}
          hint={last ? `${fmtUsdc(last.total_cum_payout)} USDC paid · ${fmtUsdc(last.total_cum_bill)} billed` : "make demo-settlement starts the loop"}
        />
      </div>

      <MapExplorer />

      <div className="grid gap-4 xl:grid-cols-2">
        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2">
              <ServerIcon className="size-4 text-primary" /> Nodes
            </CardTitle>
            <CardDescription>From ENS through the nameserver</CardDescription>
            <CardAction>
              <Button variant="ghost" size="sm" asChild>
                <Link href="/nodes/">
                  All nodes <ArrowRightIcon />
                </Link>
              </Button>
            </CardAction>
          </CardHeader>
          <CardContent className="space-y-1.5">
            {nodes.map((n) => (
              <div key={n.label} className="flex items-center gap-3 rounded-lg border bg-muted/20 px-3 py-2 text-sm">
                <StatusDot tone={!n.active ? "bad" : n.up === false ? "warn" : "ok"} pulse={n.active && n.up !== false} />
                <span className="font-mono font-medium">{n.label}</span>
                <span className="hidden font-mono text-xs text-muted-foreground sm:inline">{n.ipv4}</span>
                <span className="ml-auto flex items-center gap-2">
                  {closest[n.label] ? (
                    <Pill tone="ok">
                      <GlobeIcon className="size-3" /> closest in {closest[n.label]}
                    </Pill>
                  ) : null}
                  {inSet.has(n.label) && <Pill tone="info">slot set</Pill>}
                  <span className="font-mono text-xs text-muted-foreground">
                    {n.heartbeat ? `${n.heartbeat.age_s}s` : "—"}
                  </span>
                </span>
              </div>
            ))}
            {!nodes.length && <p className="text-sm text-muted-foreground">No nodes yet: the nameserver syncs every 10 s.</p>}
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle>Recent jobs</CardTitle>
            <CardDescription>Nodes and clients added, humans verified, attestations sent</CardDescription>
            <CardAction>
              <Button variant="ghost" size="sm" asChild>
                <Link href="/ops/">
                  Operations <ArrowRightIcon />
                </Link>
              </Button>
            </CardAction>
          </CardHeader>
          <CardContent>
            <RecentJobs limit={5} />
          </CardContent>
        </Card>
      </div>
    </>
  );
}
