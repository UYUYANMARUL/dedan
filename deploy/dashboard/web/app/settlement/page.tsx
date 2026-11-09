"use client";

import { useState } from "react";
import useSWR from "swr";
import { Area, AreaChart, CartesianGrid, XAxis, YAxis } from "recharts";
import { CoinsIcon, FileTextIcon, LandmarkIcon, ReceiptIcon, ShieldCheckIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { ChartContainer, ChartLegend, ChartLegendContent, ChartTooltip, ChartTooltipContent, type ChartConfig } from "@/components/ui/chart";
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { PageHeader } from "@/components/page-header";
import { StatCard } from "@/components/stat-card";
import { CodeBlock } from "@/components/code-block";
import { Pill } from "@/components/status";
import { fmtUsdc, short, time, usdc, useDevnet } from "@/lib/api";

const chartConfig = {
  bill: { label: "Cumulative bill", color: "var(--chart-2)" },
  payout: { label: "Cumulative payout", color: "var(--chart-1)" },
} satisfies ChartConfig;

function Report({ period, onClose }: { period: number | null; onClose: () => void }) {
  const { data } = useSWR<string>(period === null ? null : `/api/settlement/${period}`, (u: string) => fetch(u).then((r) => r.text()));
  return (
    <Sheet open={period !== null} onOpenChange={(o) => !o && onClose()}>
      <SheetContent className="flex w-full flex-col gap-3 sm:max-w-4xl">
        <SheetHeader>
          <SheetTitle>Period {period} report</SheetTitle>
          <SheetDescription className="font-mono">dedan-aggregator report /data/periods/{period}</SheetDescription>
        </SheetHeader>
        <div className="min-h-0 flex-1 overflow-auto px-4 pb-4">
          <pre className="rounded-lg border bg-black/40 p-4 font-mono text-[11px] leading-relaxed whitespace-pre">{data ?? "loading the report…"}</pre>
        </div>
      </SheetContent>
    </Sheet>
  );
}

export default function Settlement() {
  const { settlement } = useDevnet();
  const [report, setReport] = useState<number | null>(null);
  const periods = settlement?.periods ?? [];
  const last = periods.at(-1);
  const published = periods.filter((p) => p.published).length;
  const chart = periods.map((p) => ({ period: `#${p.period}`, payout: usdc(p.total_cum_payout), bill: usdc(p.total_cum_bill) }));
  return (
    <>
      <PageHeader
        title="Settlement"
        description="Every period the aggregator writes its inputs and outputs, an independent countersigner recomputes them, and publishRoot goes on chain with both signatures. Operators claim their cumulative pay with one Merkle proof once a root is final."
      >
        {settlement && <Pill tone="info">period {settlement.period_secs} s</Pill>}
      </PageHeader>

      {!settlement ? (
        <Card>
          <CardContent className="flex flex-col items-center gap-4 py-14 text-center">
            <LandmarkIcon className="size-10 text-muted-foreground/50" />
            <div className="space-y-1">
              <div className="font-medium">The settlement loop is not running</div>
              <p className="text-sm text-muted-foreground">It starts on a fresh devnet with the city auditors, the aggregator and the countersigner.</p>
            </div>
            <CodeBlock code="make devnet-reset && make demo-settlement" className="w-full max-w-md text-left" />
          </CardContent>
        </Card>
      ) : (
        <>
          <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-4">
            <StatCard label="Latest period" icon={ReceiptIcon} value={last ? `#${last.period}` : "—"} hint={last ? `${time(last.start)} – ${time(last.end)}` : `first close within ${settlement.period_secs} s`} />
            <StatCard label="Paid to nodes & auditors" icon={CoinsIcon} value={last ? fmtUsdc(last.total_cum_payout) : "0.00"} hint="USDC, cumulative" />
            <StatCard label="Billed to customers" icon={LandmarkIcon} accent="from-chart-2/15" value={last ? fmtUsdc(last.total_cum_bill) : "0.00"} hint="USDC, cumulative (payout ≤ bill ≤ deposits)" />
            <StatCard label="Roots on chain" icon={ShieldCheckIcon} accent="from-chart-3/15" value={`${published}/${periods.length}`} hint="published with two signatures" />
          </div>

          {chart.length > 1 && (
            <Card>
              <CardHeader>
                <CardTitle>Money over periods</CardTitle>
                <CardDescription>The contract enforces cumulative payouts ≤ cumulative bills at every publish</CardDescription>
              </CardHeader>
              <CardContent>
                <ChartContainer config={chartConfig} className="aspect-auto h-64 w-full">
                  <AreaChart data={chart} margin={{ left: 4, right: 12 }}>
                    <defs>
                      {(["bill", "payout"] as const).map((k) => (
                        <linearGradient key={k} id={`fill-${k}`} x1="0" y1="0" x2="0" y2="1">
                          <stop offset="5%" stopColor={`var(--color-${k})`} stopOpacity={0.5} />
                          <stop offset="95%" stopColor={`var(--color-${k})`} stopOpacity={0.02} />
                        </linearGradient>
                      ))}
                    </defs>
                    <CartesianGrid vertical={false} strokeDasharray="3 3" />
                    <XAxis dataKey="period" tickLine={false} axisLine={false} />
                    <YAxis tickLine={false} axisLine={false} width={48} unit="$" />
                    <ChartTooltip content={<ChartTooltipContent formatter={(v, n) => `${chartConfig[n as keyof typeof chartConfig].label}: ${Number(v).toFixed(2)} USDC`} />} />
                    <Area dataKey="bill" type="monotone" stroke="var(--color-bill)" fill="url(#fill-bill)" strokeWidth={2} />
                    <Area dataKey="payout" type="monotone" stroke="var(--color-payout)" fill="url(#fill-payout)" strokeWidth={2} />
                    <ChartLegend content={<ChartLegendContent />} />
                  </AreaChart>
                </ChartContainer>
              </CardContent>
            </Card>
          )}

          <Card className="py-0">
            <CardContent className="p-0">
              <Table>
                <TableHeader>
                  <TableRow className="hover:bg-transparent">
                    <TableHead className="pl-5">Period</TableHead>
                    <TableHead>Window</TableHead>
                    <TableHead className="text-right">Registry block</TableHead>
                    <TableHead className="text-right">Cumulative payout</TableHead>
                    <TableHead className="text-right">Cumulative bill</TableHead>
                    <TableHead>On chain</TableHead>
                    <TableHead className="pr-5 text-right">Report</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {periods
                    .slice()
                    .reverse()
                    .map((p) => (
                      <TableRow key={p.period}>
                        <TableCell className="pl-5 font-mono font-medium">#{p.period}</TableCell>
                        <TableCell className="font-mono text-xs whitespace-nowrap">
                          {time(p.start)} – {time(p.end)}
                        </TableCell>
                        <TableCell className="text-right font-mono text-xs">{p.registry_block}</TableCell>
                        <TableCell className="text-right font-mono text-xs">{fmtUsdc(p.total_cum_payout, 6)}</TableCell>
                        <TableCell className="text-right font-mono text-xs">{fmtUsdc(p.total_cum_bill, 6)}</TableCell>
                        <TableCell>
                          {p.published ? (
                            <Tooltip>
                              <TooltipTrigger>
                                <Pill tone="ok">published {p.tx ? short(p.tx, 8, 4) : ""}</Pill>
                              </TooltipTrigger>
                              <TooltipContent className="font-mono">
                                tx {p.tx}
                                <br />
                                payout root {p.payout_root}
                                <br />
                                bill root {p.bill_root}
                              </TooltipContent>
                            </Tooltip>
                          ) : (
                            <Pill tone="warn">not published</Pill>
                          )}
                        </TableCell>
                        <TableCell className="pr-5 text-right">
                          <Button variant="ghost" size="sm" className="h-7 gap-1.5 text-xs" onClick={() => setReport(p.period)}>
                            <FileTextIcon className="size-3.5" /> Report
                          </Button>
                        </TableCell>
                      </TableRow>
                    ))}
                  {!periods.length && (
                    <TableRow>
                      <TableCell colSpan={7} className="py-10 text-center text-muted-foreground">
                        No period closed yet (every {settlement.period_secs} s).
                      </TableCell>
                    </TableRow>
                  )}
                </TableBody>
              </Table>
            </CardContent>
          </Card>
        </>
      )}
      <Report period={report} onClose={() => setReport(null)} />
    </>
  );
}
