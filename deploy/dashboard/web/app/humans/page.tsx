"use client";

import { useMemo, useState } from "react";
import { toast } from "sonner";
import { FingerprintIcon, MapPinPlusIcon, RadarIcon, SendIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { PageHeader } from "@/components/page-header";
import { CodeBlock } from "@/components/code-block";
import { RecentJobs } from "@/components/recent-jobs";
import { Pill } from "@/components/status";
import { LogSheet, TerminalLink } from "@/components/log-sheet";
import { post, short, useDevnet } from "@/lib/api";
import { cn } from "@/lib/utils";

const presets: [string, number, number][] = [
  ["paris", 48.8566, 2.3522],
  ["saopaulo", -23.5505, -46.6333],
  ["sydney", -33.8688, 151.2093],
  ["mumbai", 19.076, 72.8777],
  ["johannesburg", -26.2041, 28.0473],
  ["losangeles", 34.0522, -118.2437],
  ["berlin", 52.52, 13.405],
  ["madrid", 40.4168, -3.7038],
  ["osaka", 34.6937, 135.5023],
  ["toronto", 43.6532, -79.3832],
];

function Step({ n, title, children, done }: { n: number; title: string; children: React.ReactNode; done?: boolean }) {
  return (
    <div className="relative flex gap-4">
      <div
        className={cn(
          "flex size-7 shrink-0 items-center justify-center rounded-full border text-xs font-semibold",
          done ? "border-ok/40 bg-ok/15 text-ok" : "border-primary/40 bg-primary/10 text-primary",
        )}
      >
        {n}
      </div>
      <div className="min-w-0 flex-1 space-y-3 pb-2">
        <div className="text-sm font-medium">{title}</div>
        {children}
      </div>
    </div>
  );
}

function AddLocation() {
  const [kind, setKind] = useState<"client" | "node">("client");
  const [label, setLabel] = useState("");
  const [lat, setLat] = useState("");
  const [lon, setLon] = useState("");
  const [busy, setBusy] = useState(false);
  const origin = typeof window === "undefined" ? "http://localhost:9090" : window.location.origin;
  const curl = `curl -X POST ${origin}/${kind}s -H 'content-type: application/json' \\\n  -d '{"label":"${label || "berlin"}","lat":${lat || "52.52"},"lon":${lon || "13.405"}}'`;
  const add = async () => {
    setBusy(true);
    try {
      await post("/clients", { label, lat: parseFloat(lat), lon: parseFloat(lon), kind });
      toast.info(`Adding ${kind} ${label}`, { description: "the job's log is below" });
    } catch (e) {
      toast.error(`Could not add ${label || "it"}`, { description: (e as Error).message });
    } finally {
      setBusy(false);
    }
  };
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <MapPinPlusIcon className="size-4 text-primary" /> Add a client or a node at a location
        </CardTitle>
        <CardDescription>Each gets its own /24 on the simulated WAN; round trips follow great-circle distance ±20 % with jitter.</CardDescription>
      </CardHeader>
      <CardContent className="space-y-5">
        <RadioGroup value={kind} onValueChange={(v) => setKind(v as "client" | "node")} className="grid gap-3 sm:grid-cols-2">
          {[
            ["client", "Client", "A human's device. Unverified until the World ID step."],
            ["node", "CDN node", "A node registered on ENS, with a local human."],
          ].map(([v, t, d]) => (
            <Label
              key={v}
              htmlFor={`kind-${v}`}
              className="flex cursor-pointer items-start gap-3 rounded-lg border p-3 has-[[data-state=checked]]:border-primary/60 has-[[data-state=checked]]:bg-primary/5"
            >
              <RadioGroupItem value={v} id={`kind-${v}`} className="mt-0.5" />
              <div className="space-y-1">
                <div className="font-medium">{t}</div>
                <div className="text-xs font-normal text-muted-foreground">{d}</div>
              </div>
            </Label>
          ))}
        </RadioGroup>
        <div className="grid gap-3 sm:grid-cols-2">
          <Select
            onValueChange={(v) => {
              const p = presets.find((x) => x[0] === v)!;
              setLabel(p[0]);
              setLat(String(p[1]));
              setLon(String(p[2]));
            }}
          >
            <SelectTrigger className="w-full">
              <SelectValue placeholder="Preset city" />
            </SelectTrigger>
            <SelectContent>
              {presets.map(([l]) => (
                <SelectItem key={l} value={l}>
                  {l}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <Input placeholder="label (a-z, digits, dashes)" value={label} onChange={(e) => setLabel(e.target.value)} />
        </div>
        <div className="grid grid-cols-[1fr_1fr_auto] gap-3">
          <Input placeholder="latitude" value={lat} onChange={(e) => setLat(e.target.value)} />
          <Input placeholder="longitude" value={lon} onChange={(e) => setLon(e.target.value)} />
          <Button onClick={add} disabled={busy || !label || !lat || !lon}>
            Add {kind}
          </Button>
        </div>
        <CodeBlock code={curl} />
      </CardContent>
    </Card>
  );
}

function Attest() {
  const { humans, cells, jobs } = useDevnet();
  const [chosenCity, setCity] = useState<string>("");
  const [picked, setPicked] = useState<Record<string, boolean>>({});
  const city = chosenCity || humans[0]?.city || "";
  const sel = humans.find((h) => h.city === city);
  const targets = useMemo(() => cells.find((c) => sel && c.cell === sel.cell)?.members ?? [], [cells, sel]);
  const verifiedBy = useMemo(() => {
    for (const j of Object.values(jobs).reverse())
      if (j.kind === "worldid" && j.status === "done" && j.label === city) return j.log.match(/verified: (0x[0-9a-fA-F]+)/)?.[1];
  }, [jobs, city]);
  const verified = !!verifiedBy || !!sel?.verified_at_start;
  const chosen = targets.filter((t) => picked[t] !== false);
  const origin = typeof window === "undefined" ? "http://localhost:9090" : window.location.origin;
  const run = async (path: string, body: object, what: string) => {
    try {
      await post(path, body);
      toast.info(what, { description: "the job's log is below" });
    } catch (e) {
      toast.error(`${what}: refused`, { description: (e as Error).message });
    }
  };
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <RadarIcon className="size-4 text-primary" /> Submit node metrics from a client
        </CardTitle>
        <CardDescription>
          The client pings the nodes that joined its cell, fetches a token from each, and sends one batch to DedanCells.attest(cell, batch),
          signed by the human&apos;s key: one vote per node, at most one batch a minute per cell. Without the attester role the contract rejects it.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-5">
        <div className="space-y-2">
          <div className="flex items-center justify-between">
            <Label>Client</Label>
            {city && (
              <div className="flex gap-1">
                <TerminalLink name={`user-${city}`} />
                <LogSheet name={`user-${city}`} />
              </div>
            )}
          </div>
          <Select value={city} onValueChange={(v) => { setCity(v); setPicked({}); }}>
            <SelectTrigger className="w-full">
              <SelectValue placeholder="Pick a client" />
            </SelectTrigger>
            <SelectContent>
              {humans.map((h) => (
                <SelectItem key={h.city} value={h.city}>
                  <span className="font-medium">{h.city}</span>
                  <span className="text-muted-foreground">
                    {h.kind === "client" ? "client" : "city user"} · {h.ip} · human key {h.attester}
                  </span>
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        <Step n={1} title="Verify the human (World ID mock)" done={verified}>
          <div className="flex flex-wrap items-center gap-3">
            <Button variant="secondary" disabled={!city} onClick={() => run("/api/worldid", { city }, `Verifying the human of ${city}`)}>
              <FingerprintIcon /> Verify with World ID (mock)
            </Button>
            {verifiedBy ? (
              <Pill tone="ok">verified {short(verifiedBy)} · holds ROLE_ATTESTER</Pill>
            ) : sel?.verified_at_start ? (
              <Pill tone="ok">verified at devnet start</Pill>
            ) : (
              <Pill tone="warn">not verified: the contract will reject its metrics</Pill>
            )}
          </div>
          <p className="text-xs text-muted-foreground">
            The real flow, with IDKit and a Proof of Human credential, is on the World ID page.
          </p>
        </Step>
        <Step n={2} title="Measure from the client and submit">
          {targets.length ? (
            <div className="flex flex-wrap gap-2">
              {targets.map((t) => (
                <Label key={t} className="flex cursor-pointer items-center gap-2 rounded-md border px-3 py-1.5 font-mono text-xs">
                  <Checkbox checked={picked[t] !== false} onCheckedChange={(v) => setPicked({ ...picked, [t]: !!v })} />
                  {t}
                </Label>
              ))}
            </div>
          ) : (
            <p className="text-sm text-muted-foreground">No node has joined this client&apos;s cell: nothing to attest.</p>
          )}
          <Button disabled={!city || !chosen.length} onClick={() => run("/api/attest", { city, nodes: chosen }, `Attesting from ${city}`)}>
            <SendIcon /> Measure and submit {chosen.length ? `(${chosen.length})` : ""}
          </Button>
        </Step>
        <CodeBlock
          code={`curl -X POST ${origin}/api/worldid -H 'content-type: application/json' -d '{"city":"${city || "berlin"}"}'   # once\ncurl -X POST ${origin}/api/attest -H 'content-type: application/json' -d '{"city":"${city || "berlin"}"}'`}
        />
      </CardContent>
    </Card>
  );
}

export default function Humans() {
  return (
    <>
      <PageHeader
        title="Clients & attestation"
        description="Put a human's device or a new CDN node anywhere on the map, verify the human, and send their measurements to the contract. Accepted votes move the medians within about 12 s."
      />
      <div className="grid gap-4 xl:grid-cols-2">
        <AddLocation />
        <Attest />
      </div>
      <Card>
        <CardHeader>
          <CardTitle>Jobs</CardTitle>
          <CardDescription>What the buttons above ran, with their output</CardDescription>
        </CardHeader>
        <CardContent>
          <RecentJobs kinds={["add", "worldid", "attest"]} limit={8} />
        </CardContent>
      </Card>
    </>
  );
}
