"use client";

import { useState } from "react";
import { SearchIcon } from "lucide-react";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { PageHeader } from "@/components/page-header";
import { LogSheet, TerminalLink } from "@/components/log-sheet";
import { RecentJobs } from "@/components/recent-jobs";
import { Pill, StatusDot } from "@/components/status";
import { shortContainer, useDevnet } from "@/lib/api";

export default function Ops() {
  const { containers } = useDevnet();
  const [q, setQ] = useState("");
  const shown = containers.filter((c) => c.name.includes(q));
  const up = containers.filter((c) => c.status.startsWith("Up")).length;
  return (
    <>
      <PageHeader title="Operations" description="The dashboard's jobs with their output, and every devnet container with its logs and a shell.">
        <Pill tone={up === containers.length ? "ok" : "warn"}>
          {up}/{containers.length} containers up
        </Pill>
      </PageHeader>
      <div className="grid gap-4 xl:grid-cols-[minmax(0,1fr)_minmax(0,1fr)]">
        <Card>
          <CardHeader>
            <CardTitle>Jobs</CardTitle>
            <CardDescription>Scripts the dashboard ran, newest first</CardDescription>
          </CardHeader>
          <CardContent>
            <RecentJobs />
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle>Containers</CardTitle>
            <CardDescription>docker ps for the dedan-devnet project</CardDescription>
            <div className="relative pt-2">
              <SearchIcon className="absolute top-1/2 left-2.5 mt-1 size-4 -translate-y-1/2 text-muted-foreground" />
              <Input placeholder="Filter containers" className="pl-8" value={q} onChange={(e) => setQ(e.target.value)} />
            </div>
          </CardHeader>
          <CardContent>
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Container</TableHead>
                  <TableHead>Status</TableHead>
                  <TableHead className="text-right">Open</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {shown.map((c) => {
                  const name = shortContainer(c.name);
                  const tone = c.status.startsWith("Up") ? (c.status.includes("unhealthy") ? "warn" : "ok") : "bad";
                  return (
                    <TableRow key={c.id}>
                      <TableCell>
                        <div className="flex items-center gap-2.5">
                          <StatusDot tone={tone} />
                          <span className="font-mono text-xs">{name}</span>
                        </div>
                      </TableCell>
                      <TableCell className="text-xs text-muted-foreground">{c.status}</TableCell>
                      <TableCell>
                        <div className="flex justify-end gap-1">
                          <LogSheet name={name} />
                          <TerminalLink name={name} />
                        </div>
                      </TableCell>
                    </TableRow>
                  );
                })}
              </TableBody>
            </Table>
          </CardContent>
        </Card>
      </div>
    </>
  );
}
