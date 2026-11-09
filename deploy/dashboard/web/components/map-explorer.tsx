"use client";

import { useState } from "react";
import dynamic from "next/dynamic";
import { MousePointerClickIcon } from "lucide-react";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { ScrollArea } from "@/components/ui/scroll-area";
import { short, time, useDevnet } from "@/lib/api";
import { Pill, StatusDot, type Tone } from "@/components/status";
import type { Selection } from "@/components/world-map";
import { LogSheet, TerminalLink } from "@/components/log-sheet";
import { cn } from "@/lib/utils";

const WorldMap = dynamic(() => import("@/components/world-map"), {
  ssr: false,
  loading: () => <Skeleton className="size-full rounded-none" />,
});

const legend: [Tone, string][] = [
  ["ok", "cell ≤ 30 ms"],
  ["warn", "cell ≤ 120 ms"],
  ["bad", "cell > 120 ms"],
  ["info", "cell without votes"],
];

function Actions({ container }: { container: string }) {
  return (
    <div className="flex gap-1">
      <TerminalLink name={container} />
      <LogSheet name={container} />
    </div>
  );
}

function Detail({ selection, onSelect }: { selection: Selection; onSelect: (s: Selection) => void }) {
  const { cells, humans } = useDevnet();
  if (selection.client) {
    const h = humans.find((x) => x.city === selection.client);
    if (!h) return null;
    const cell = cells.find((c) => c.cell === h.cell);
    return (
      <div className="space-y-3">
        <div>
          <div className="font-mono text-sm font-medium">user-{h.city}</div>
          <div className="text-xs text-muted-foreground">
            {h.kind === "client" ? "a client device added at runtime" : "the city's user"} · {h.ip} · human key {h.attester}
          </div>
        </div>
        <Actions container={`user-${h.city}`} />
        <div className="space-y-1.5">
          <div className="text-xs text-muted-foreground">Its cell, and the nodes it measures there</div>
          {cell ? (
            <button type="button" onClick={() => onSelect({ cell: cell.cell })} className="w-full rounded-lg border bg-muted/30 p-2.5 text-left hover:bg-muted/60">
              <div className="font-mono text-xs break-all">{cell.name}</div>
              <div className="mt-2 flex flex-wrap gap-1.5">
                {cell.members.map((m) => {
                  const r = cell.nodes.find((x) => x.label === m);
                  return (
                    <Pill key={m} tone={r ? "ok" : "none"}>
                      {m} {r ? `${r.median_ms} ms` : "no votes"}
                    </Pill>
                  );
                })}
              </div>
            </button>
          ) : (
            <p className="text-xs text-muted-foreground">No node has joined this client&apos;s cell yet: nothing to measure.</p>
          )}
        </div>
      </div>
    );
  }
  if (selection.node) {
    const mine = cells.filter((c) => c.members.includes(selection.node!));
    return (
      <div className="space-y-3">
        <div>
          <div className="font-mono text-sm font-medium">{selection.node}.dedan.eth</div>
          <div className="text-xs text-muted-foreground">
            joined {mine.length} cell{mine.length === 1 ? "" : "s"}
          </div>
        </div>
        <Actions container={selection.node} />
        <div className="space-y-1.5">
          {mine.map((c) => {
            const r = c.nodes.find((x) => x.label === selection.node);
            return (
              <button
                type="button"
                key={c.cell}
                onClick={() => onSelect({ cell: c.cell })}
                className="flex w-full items-center justify-between gap-2 rounded-md border bg-muted/30 px-2.5 py-1.5 text-left hover:bg-muted/60"
              >
                <span className="truncate font-mono text-xs">{selection.node}.{c.name.replace(/\.dedan\.eth$/, "")}</span>
                {r ? <Pill tone="ok">{r.median_ms} ms</Pill> : <Pill tone="none">no votes</Pill>}
              </button>
            );
          })}
        </div>
      </div>
    );
  }
  const c = cells.find((x) => x.cell === selection.cell);
  if (!c)
    return (
      <div className="flex h-full flex-col items-center justify-center gap-2 py-10 text-center text-sm text-muted-foreground">
        <MousePointerClickIcon className="size-6" />
        Click a cell for its members and on-chain medians, a node (dot) or a client (ring) for its cells, terminal and logs.
      </div>
    );
  const city = humans.find((h) => h.cell === c.cell)?.city;
  return (
    <div className="space-y-3">
      <div>
        <div className="font-mono text-sm font-medium break-all">{c.name}</div>
        <div className="text-xs text-muted-foreground">
          {city && <span className="text-foreground">{city} · </span>}
          centre {c.centre.lat.toFixed(2)}, {c.centre.lon.toFixed(2)} · {c.humans} vote{c.humans === 1 ? "" : "s"} · {c.members.length} member
          {c.members.length === 1 ? "" : "s"}
        </div>
      </div>
      {c.members.map((m) => {
        const r = c.nodes.find((x) => x.label === m);
        return (
          <div key={m} className="rounded-lg border bg-muted/30 p-2.5">
            <div className="flex items-center justify-between gap-2">
              <span className="font-mono text-xs">{m}</span>
              {r ? (
                <Pill tone="ok">
                  {r.median_ms} ms · {r.voters.length} vote{r.voters.length === 1 ? "" : "s"}
                </Pill>
              ) : (
                <Pill tone="none">no votes yet</Pill>
              )}
            </div>
            {r && r.voters.length > 0 && (
              <div className="mt-2 space-y-1 border-t pt-2">
                {r.voters.map((v) => (
                  <div key={v.attester} className="flex justify-between font-mono text-[11px] text-muted-foreground">
                    <span>{short(v.attester)}</span>
                    <span className="text-foreground">{v.rtt_ms} ms</span>
                    <span>{time(v.ts)}</span>
                  </div>
                ))}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}

// literal class names, so Tailwind sees them
const HEIGHTS = {
  normal: { map: "h-[420px] sm:h-[520px]", side: "max-h-[420px] lg:max-h-none lg:h-[520px]" },
  tall: { map: "h-[460px] sm:h-[600px]", side: "max-h-[460px] lg:max-h-none lg:h-[600px]" },
};

export function MapExplorer({ tall = false, title = "Cells and nodes" }: { tall?: boolean; title?: string }) {
  const h = HEIGHTS[tall ? "tall" : "normal"];
  const { cells, nodes, humans } = useDevnet();
  const [selection, setSelection] = useState<Selection>({});
  return (
    <div className="grid gap-4 lg:grid-cols-[minmax(0,1fr)_340px]">
      <Card className="gap-0 overflow-hidden py-0">
        <div className={cn("relative", h.map)}>
          <WorldMap cells={cells} nodes={nodes} humans={humans} selection={selection} onSelect={setSelection} />
          <div className="pointer-events-none absolute top-3 right-3 z-[500] flex flex-col gap-1.5 rounded-lg border bg-background/80 px-3 py-2 text-xs backdrop-blur">
            <span className="flex items-center gap-2">
              <span className="size-2 rounded-full bg-primary shadow-[0_0_8px] shadow-primary" /> CDN node
            </span>
            <span className="flex items-center gap-2">
              <span className="size-2.5 rounded-full border-2 border-dashed border-chart-2" /> client device
            </span>
            {legend.map(([tone, label]) => (
              <span key={label} className="flex items-center gap-2">
                <StatusDot tone={tone} /> {label}
              </span>
            ))}
          </div>
        </div>
      </Card>
      <Card className={cn("gap-3", h.side)}>
        <CardHeader>
          <CardTitle>{title}</CardTitle>
          <CardDescription>Medians are the contract&apos;s, one vote per human per node per cell.</CardDescription>
        </CardHeader>
        <CardContent className="min-h-0 flex-1">
          <ScrollArea className="h-full pr-3">
            <Detail selection={selection} onSelect={setSelection} />
          </ScrollArea>
        </CardContent>
      </Card>
    </div>
  );
}
