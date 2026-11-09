"use client";

import { Card, CardContent } from "@/components/ui/card";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { PageHeader } from "@/components/page-header";
import { LogSheet, TerminalLink } from "@/components/log-sheet";
import { Pill, StatusDot } from "@/components/status";
import { fmtUsdc, short, useDevnet } from "@/lib/api";

export default function Nodes() {
  const { nodes, ranks, sets } = useDevnet();
  const closest: Record<string, number> = {};
  for (const r of ranks) if (r.rings === 0 && r.nodes.length) closest[r.nodes[0][0]] = (closest[r.nodes[0][0]] ?? 0) + 1;
  const accountsOf = (label: string) => Object.entries(sets).filter(([, s]) => s.includes(label)).map(([a]) => a);
  return (
    <>
      <PageHeader
        title="Nodes"
        description="Every <label>.dedan.eth name the nameserver resolved from ENS: its records, its stake, its heartbeat, the accounts whose slot set it sits in and the cells where it is the fastest."
      />
      <Card className="py-0">
        <CardContent className="p-0">
          <Table>
            <TableHeader>
              <TableRow className="hover:bg-transparent">
                <TableHead className="pl-5">Node</TableHead>
                <TableHead>IPv4</TableHead>
                <TableHead>Location</TableHead>
                <TableHead>Class</TableHead>
                <TableHead className="text-right">Stake</TableHead>
                <TableHead>State</TableHead>
                <TableHead>Slot sets</TableHead>
                <TableHead>Closest for</TableHead>
                <TableHead className="pr-5 text-right">Container</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {nodes.map((n) => {
                const tone = !n.active ? "bad" : n.up === false ? "warn" : "ok";
                return (
                  <TableRow key={n.label}>
                    <TableCell className="pl-5">
                      <div className="flex items-center gap-2.5">
                        <StatusDot tone={tone} pulse={tone === "ok"} />
                        <div>
                          <div className="font-mono text-sm font-medium">{n.name}</div>
                          <Tooltip>
                            <TooltipTrigger className="font-mono text-xs text-muted-foreground">owner {short(n.owner)}</TooltipTrigger>
                            <TooltipContent className="font-mono">
                              owner {n.owner}
                              <br />
                              payout {n.payout}
                              <br />
                              signers {n.signers.join(", ")}
                            </TooltipContent>
                          </Tooltip>
                        </div>
                      </div>
                    </TableCell>
                    <TableCell className="font-mono text-xs">
                      {n.ipv4 ?? "—"}
                      {n.ipv4 && n.verified === false && <div className="text-warn">endpoint unverified</div>}
                    </TableCell>
                    <TableCell className="font-mono text-xs whitespace-nowrap">
                      {n.location ? `${n.location.lat.toFixed(2)}, ${n.location.lon.toFixed(2)}` : "—"}
                    </TableCell>
                    <TableCell>
                      <Pill tone="none">{n.class}</Pill>
                    </TableCell>
                    <TableCell className="text-right font-mono text-xs whitespace-nowrap">{fmtUsdc(n.stake, 0)} USDC</TableCell>
                    <TableCell>
                      <div className="flex flex-col items-start gap-1">
                        <Pill tone={n.active ? "ok" : "bad"}>{n.active ? "active" : "inactive"}</Pill>
                        <span className="text-xs text-muted-foreground">
                          {n.up === false ? <span className="text-warn">no heartbeat</span> : n.heartbeat ? `heartbeat ${n.heartbeat.age_s}s ago` : ""}
                        </span>
                      </div>
                    </TableCell>
                    <TableCell>
                      <div className="flex flex-wrap gap-1">
                        {accountsOf(n.label).map((a) => (
                          <Pill key={a} tone="info">
                            {a}
                          </Pill>
                        ))}
                        {!accountsOf(n.label).length && <span className="text-xs text-muted-foreground">—</span>}
                      </div>
                    </TableCell>
                    <TableCell className="text-sm">
                      {closest[n.label] ? (
                        `${closest[n.label]} cell${closest[n.label] > 1 ? "s" : ""}`
                      ) : (
                        <span className="text-xs text-muted-foreground">no cell</span>
                      )}
                    </TableCell>
                    <TableCell className="pr-5">
                      <div className="flex justify-end gap-1">
                        <LogSheet name={n.label} />
                        <TerminalLink name={n.label} />
                      </div>
                    </TableCell>
                  </TableRow>
                );
              })}
              {!nodes.length && (
                <TableRow>
                  <TableCell colSpan={9} className="py-10 text-center text-muted-foreground">
                    No nodes yet: the nameserver syncs every 10 s.
                  </TableCell>
                </TableRow>
              )}
            </TableBody>
          </Table>
        </CardContent>
      </Card>
    </>
  );
}
