"use client";

import { useState } from "react";
import { Bar, BarChart, CartesianGrid, Cell as BarCell, XAxis, YAxis } from "recharts";
import { ChevronRightIcon, ShieldAlertIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { ChartContainer, ChartTooltip, ChartTooltipContent, type ChartConfig } from "@/components/ui/chart";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { PageHeader } from "@/components/page-header";
import { MapExplorer } from "@/components/map-explorer";
import { Pill } from "@/components/status";
import { bestMs, latencyTone, short, time, useDevnet } from "@/lib/api";

const chartConfig = { ms: { label: "Best median (ms)", color: "var(--chart-1)" } } satisfies ChartConfig;
const toneVar = { ok: "var(--ok)", warn: "var(--warn)", bad: "var(--bad)", none: "var(--chart-1)" } as const;

function Medians({ items }: { items: [string, number][] }) {
  if (!items.length) return <span className="text-xs text-muted-foreground">no votes yet</span>;
  return (
    <div className="flex flex-wrap gap-1.5">
      {items.map(([n, ms], i) => (
        <Pill key={n} tone={i === 0 ? "ok" : "none"}>
          {n} <span className="font-mono">{ms}</span>
        </Pill>
      ))}
    </div>
  );
}

export default function Network() {
  const { cells, ranks, rejected, humans } = useDevnet();
  const [allCells, setAllCells] = useState(false);
  const voted = cells.filter((c) => c.nodes.length);
  const listed = allCells ? [...voted, ...cells.filter((c) => !c.nodes.length)] : voted;
  const cityOf: Record<string, string> = Object.fromEntries(humans.map((h) => [h.cell, h.city]));
  const label = (cell: string, name?: string) => cityOf[cell] ?? (name ?? cell).replace(/f+\.dedan\.eth$/, "…");
  const chart = cells
    .filter((c) => c.nodes.length)
    .map((c) => ({ cell: label(c.cell, c.name), ms: bestMs(c)!, node: c.nodes[0].label, tone: latencyTone(bestMs(c)) }))
    .sort((a, b) => a.ms - b.ms);
  const reason = (r: (typeof rejected)[number]["reason"]) => (typeof r === "string" ? r : Object.keys(r)[0]);
  const byReason: Record<string, number> = {};
  for (const e of rejected) byReason[reason(e.reason)] = (byReason[reason(e.reason)] ?? 0) + 1;
  const own = ranks.filter((r) => r.rings === 0);
  return (
    <>
      <PageHeader
        title="Latency map"
        description="Each hexagon is a resolution-2 cell that a node joined. Verified humans in the cell measure its members; the contract keeps one vote per human per node and publishes the median as the cell's records."
      />
      <MapExplorer tall title="Selection" />

      <div className="grid gap-4 xl:grid-cols-[minmax(0,1fr)_minmax(0,1.2fr)]">
        <Card>
          <CardHeader>
            <CardTitle>Fastest node per cell</CardTitle>
            <CardDescription>The best median on chain in every cell with votes</CardDescription>
          </CardHeader>
          <CardContent>
            {chart.length ? (
              <ChartContainer config={chartConfig} className="aspect-auto w-full" style={{ height: Math.max(180, chart.length * 38) }}>
                <BarChart data={chart} layout="vertical" margin={{ left: 8, right: 24 }}>
                  <CartesianGrid horizontal={false} strokeDasharray="3 3" />
                  <XAxis type="number" dataKey="ms" unit=" ms" tickLine={false} axisLine={false} />
                  <YAxis type="category" dataKey="cell" width={96} tickLine={false} axisLine={false} />
                  <ChartTooltip
                    cursor={{ fill: "var(--muted)", opacity: 0.4 }}
                    content={<ChartTooltipContent formatter={(v, _n, p) => `${p.payload.node}: ${v} ms`} hideIndicator />}
                  />
                  <Bar dataKey="ms" radius={6}>
                    {chart.map((d) => (
                      <BarCell key={d.cell} fill={toneVar[d.tone]} />
                    ))}
                  </Bar>
                </BarChart>
              </ChartContainer>
            ) : (
              <p className="py-10 text-center text-sm text-muted-foreground">No attestations yet: run make demo, or attest from a client.</p>
            )}
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>What the nameserver steers by</CardTitle>
            <CardDescription>Each human&apos;s own cell, its nodes ranked by median round trip</CardDescription>
          </CardHeader>
          <CardContent>
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Cell</TableHead>
                  <TableHead>City</TableHead>
                  <TableHead className="text-right">Humans</TableHead>
                  <TableHead>Ranked nodes (ms)</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {own.map((r) => (
                  <TableRow key={r.cell}>
                    <TableCell className="font-mono text-xs">{short(r.cell, 8, 4)}</TableCell>
                    <TableCell>{cityOf[r.cell] ?? ""}</TableCell>
                    <TableCell className="text-right tabular-nums">{r.humans}</TableCell>
                    <TableCell>
                      <Medians items={r.nodes} />
                    </TableCell>
                  </TableRow>
                ))}
                {!own.length && (
                  <TableRow>
                    <TableCell colSpan={4} className="text-muted-foreground">
                      No attestations yet (run make demo, or add a node).
                    </TableCell>
                  </TableRow>
                )}
              </TableBody>
            </Table>
          </CardContent>
        </Card>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Cells</CardTitle>
          <CardDescription>Resolution-2 regions as ENS names: nodes join them, humans attest their members</CardDescription>
          <CardAction>
            <Button variant="outline" size="sm" onClick={() => setAllCells(!allCells)}>
              {allCells ? "Only cells with votes" : `Show all ${cells.length}`}
            </Button>
          </CardAction>
        </CardHeader>
        <CardContent>
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Cell name</TableHead>
                <TableHead>Centre</TableHead>
                <TableHead>Members</TableHead>
                <TableHead className="text-right">Votes</TableHead>
                <TableHead>Medians on chain (ms)</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {listed.map((c) => (
                <TableRow key={c.cell}>
                  <TableCell>
                    <div className="font-mono text-xs">{c.name}</div>
                    {cityOf[c.cell] && <div className="text-xs text-muted-foreground">{cityOf[c.cell]}</div>}
                  </TableCell>
                  <TableCell className="font-mono text-xs whitespace-nowrap">
                    {c.centre.lat.toFixed(2)}, {c.centre.lon.toFixed(2)}
                  </TableCell>
                  <TableCell>
                    <div className="flex flex-wrap gap-1">
                      {c.members.map((m) => (
                        <Pill key={m} tone="info">
                          {m}
                        </Pill>
                      ))}
                    </div>
                  </TableCell>
                  <TableCell className="text-right tabular-nums">{c.humans}</TableCell>
                  <TableCell>
                    <Medians items={c.nodes.map((n) => [n.label, n.median_ms])} />
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </CardContent>
      </Card>

      {rejected.length > 0 && (
        <Card>
          <Collapsible>
            <CardHeader>
              <CollapsibleTrigger className="group flex items-center gap-2 text-left">
                <ChevronRightIcon className="size-4 transition-transform group-data-[state=open]:rotate-90" />
                <ShieldAlertIcon className="size-4 text-warn" />
                <CardTitle>
                  {rejected.length} rejected attestation{rejected.length === 1 ? "" : "s"}
                </CardTitle>
              </CollapsibleTrigger>
              <CardDescription className="flex flex-wrap gap-1.5 pt-1">
                {Object.entries(byReason).map(([k, n]) => (
                  <Pill key={k} tone="warn">
                    {k} ×{n}
                  </Pill>
                ))}
              </CardDescription>
            </CardHeader>
            <CollapsibleContent>
              <CardContent>
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Reason</TableHead>
                      <TableHead>Human</TableHead>
                      <TableHead>Node</TableHead>
                      <TableHead>Cell</TableHead>
                      <TableHead>When</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {rejected
                      .slice(-40)
                      .reverse()
                      .map((e, i) => (
                        <TableRow key={i}>
                          <TableCell className="max-w-md font-mono text-xs break-all whitespace-normal">{JSON.stringify(e.reason)}</TableCell>
                          <TableCell className="font-mono text-xs">{short(e.auditor)}</TableCell>
                          <TableCell>{e.node ?? ""}</TableCell>
                          <TableCell>{cityOf[e.cell] ?? short(e.cell, 8, 4)}</TableCell>
                          <TableCell className="text-xs text-muted-foreground">{time(e.ts)}</TableCell>
                        </TableRow>
                      ))}
                  </TableBody>
                </Table>
              </CardContent>
            </CollapsibleContent>
          </Collapsible>
        </Card>
      )}
    </>
  );
}
