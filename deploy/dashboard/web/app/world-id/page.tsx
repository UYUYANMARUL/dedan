"use client";

import { useRef, useState } from "react";
import Script from "next/script";
import { QRCodeSVG } from "qrcode.react";
import {
  CheckIcon,
  ExternalLinkIcon,
  FingerprintIcon,
  Loader2Icon,
  ScanLineIcon,
  ShieldCheckIcon,
  XIcon,
} from "lucide-react";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { PageHeader } from "@/components/page-header";
import { RecentJobs } from "@/components/recent-jobs";
import { Pill, type Tone } from "@/components/status";
import { post, useJSON } from "@/lib/api";
import { cn } from "@/lib/utils";

// IDKit 4 browser build: `window.IDKit`, its WASM loads from the same CDN directory.
const IDKIT = "https://cdn.jsdelivr.net/npm/@worldcoin/idkit-core@4.3.0/dist/idkit.global.js";
type IDKitRequest = {
  connectorURI: string;
  pollUntilCompletion(o: { pollInterval: number; timeout: number; signal: AbortSignal }): Promise<{ success: boolean; result?: unknown; error?: string }>;
};
declare global {
  interface Window {
    IDKit?: {
      request(o: Record<string, unknown>): { constraints(c: unknown): IDKitRequest };
      CredentialRequest(type: string, o: { signal: string }): unknown;
    };
  }
}

const OUTCOMES: Record<string, string> = {
  cancelled: "Cancelled: no role was granted.",
  user_rejected: "Declined in World App: no role was granted.",
  credential_unavailable: "This World ID has no Proof of Human credential. Dedan roles need one (verify at an Orb), then try again.",
  world_id_4_not_available: "World App is not on World ID 4.0 yet: update it and try again.",
  max_verifications_reached: "This human already used this action: one key per human per role.",
  nullifier_replayed: "This human already used this action: one key per human per role.",
  verification_rejected: "World App rejected the verification.",
  connection_failed: "Could not reach World App: try again.",
  timeout: "No answer from World App within three minutes: no role was granted.",
  rp_signature_expired: "The request expired (five minutes): start again.",
  timestamp_too_old: "The request expired (five minutes): start again.",
  invalid_rp_signature: "World App refused the request signature: check the app in deploy/devnet/world-id.env.",
  unknown_rp: "World App does not know this relying party: check the app in deploy/devnet/world-id.env.",
  inactive_rp: "This app is not active in the Developer Portal: enable World ID 4.0 for it.",
};

const ROLES = [
  { value: "attester", title: "Attester", body: "Votes round trips for the nodes of their cell." },
  { value: "auditor", title: "Auditor", body: "Runs assigned probes and is paid per valid one. Stakes 100 USDC." },
  { value: "operator", title: "Node operator", body: "Registers CDN nodes under <label>.dedan.eth." },
] as const;

const STAGES = ["Request signed by the control plane", "Consent in World App", "Verified at the Developer Portal", "Role granted on chain"];

type Config = { configured: boolean; app_id?: string; environment?: string; roles: Record<string, string> };

export default function WorldId() {
  const { data: config, error: configError } = useJSON<Config>("/api/worldid/config");
  const [role, setRole] = useState<string>("attester");
  const [index, setIndex] = useState(27);
  const { data: account } = useJSON<{ account: string }>(index >= 0 && index < 60 ? `/api/worldid/account?index=${index}` : null);
  const [ready, setReady] = useState(false);
  const [stage, setStage] = useState(-1);
  const [busy, setBusy] = useState(false);
  const [uri, setUri] = useState<string | null>(null);
  const [status, setStatus] = useState<{ tone: Tone; title: string; text: string } | null>(null);
  const [log, setLog] = useState<string | null>(null);
  const abort = useRef<AbortController | null>(null);

  const say = (tone: Tone, title: string, text: string) => setStatus({ tone, title, text });

  const start = async () => {
    if (!window.IDKit || !account) return;
    setBusy(true);
    setUri(null);
    setLog(null);
    setStage(0);
    try {
      say("info", "Requesting", "Asking the control plane for a signed request…");
      const req = await post<{ app_id: string; action: string; environment: string; action_description: string; rp_context: unknown }>(
        "/api/worldid/request",
        { role },
      );
      const request = window.IDKit.request({
        app_id: req.app_id,
        action: req.action,
        rp_context: req.rp_context,
        environment: req.environment,
        action_description: req.action_description,
        allow_legacy_proofs: false,
      }).constraints(window.IDKit.CredentialRequest("proof_of_human", { signal: account.account }));
      setUri(request.connectorURI);
      setStage(1);
      say(
        "info",
        "Waiting for World App",
        `Scan the code with World App${req.environment === "staging" ? " (or paste the link into simulator.worldcoin.org)" : ""}: Proof of Human, action ${req.action}, signal ${account.account}.`,
      );
      abort.current = new AbortController();
      const done = await request.pollUntilCompletion({ pollInterval: 2000, timeout: 180000, signal: abort.current.signal });
      setUri(null);
      if (!done.success) {
        const code = abort.current.signal.aborted ? "cancelled" : (done.error ?? "unknown");
        say("warn", "No proof", OUTCOMES[code] ?? `World ID ended without a proof: ${code}. No role was granted.`);
        setStage(-1);
        return;
      }
      setStage(2);
      say("info", "Verifying", "Proof received: the control plane verifies it at the Developer Portal…");
      const v = await post<{ nullifier: string; account: string; watch: string }>("/api/worldid/verify", { role, index, result: done.result });
      setStage(3);
      say("ok", "Verified", `Verified by the Developer Portal (nullifier ${v.nullifier.slice(0, 14)}…): registering ${v.account} as ${role} on chain…`);
      for (;;) {
        const job = await (await fetch(v.watch)).json();
        if (job.status !== "running") {
          setLog(job.log);
          if (job.status === "done") {
            setStage(4);
            say("ok", "Role granted", `${v.account} holds the ${role} role on chain.`);
          } else say("bad", "Transaction failed", "The registration transaction failed: see the log.");
          break;
        }
        await new Promise((r) => setTimeout(r, 1500));
      }
    } catch (e) {
      setUri(null);
      setStage(-1);
      say("bad", "Rejected", `${(e as Error).message}. No role was granted.`);
    } finally {
      setBusy(false);
      abort.current = null;
    }
  };

  const configured = !!config?.configured;
  return (
    <>
      <Script src={IDKIT} strategy="afterInteractive" onReady={() => setReady(true)} />
      <PageHeader
        title="World ID"
        description="Granting a role is the one moment Dedan must know that a key belongs to a unique human who holds no other key for that role. Attesters' votes steer traffic and auditors' probes move money."
      >
        {config &&
          (configured ? (
            <Pill tone="ok">
              {config.app_id} · {config.environment}
            </Pill>
          ) : (
            <Pill tone="warn">not configured</Pill>
          ))}
        <Pill tone={ready ? "ok" : "none"}>IDKit 4.3.0 {ready ? "loaded" : "loading"}</Pill>
      </PageHeader>

      {config && !configured && (
        <Alert>
          <FingerprintIcon />
          <AlertTitle>World ID is not configured</AlertTitle>
          <AlertDescription>
            Put the Developer Portal app&apos;s app_id, rp_id and RP signing key in deploy/devnet/world-id.env (see WORLD-ID.md), then run make
            devnet-reset. The signing key stays in the control plane; the browser never sees it.
          </AlertDescription>
        </Alert>
      )}
      {configError && (
        <Alert variant="destructive">
          <AlertTitle>Control plane unreachable</AlertTitle>
          <AlertDescription>{String(configError.message)}</AlertDescription>
        </Alert>
      )}

      <div className="grid gap-4 xl:grid-cols-[minmax(0,1.1fr)_minmax(0,0.9fr)]">
        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2">
              <FingerprintIcon className="size-4 text-primary" /> Verify with World ID
            </CardTitle>
            <CardDescription>Proof of Human only, with the account as the signal, so the proof cannot be reused for another key.</CardDescription>
          </CardHeader>
          <CardContent className="space-y-5">
            <div className="grid gap-2 sm:grid-cols-3">
              {ROLES.map((r) => (
                <button
                  key={r.value}
                  type="button"
                  onClick={() => setRole(r.value)}
                  disabled={busy}
                  className={cn(
                    "rounded-lg border p-3 text-left transition-colors hover:bg-muted/40",
                    role === r.value && "border-primary/60 bg-primary/5 ring-1 ring-primary/30",
                  )}
                >
                  <div className="text-sm font-medium">{r.title}</div>
                  <div className="mt-1 text-xs text-muted-foreground">{r.body}</div>
                  <div className="mt-2 font-mono text-[10px] text-muted-foreground">dedan-{r.value}</div>
                </button>
              ))}
            </div>
            <div className="grid items-end gap-3 sm:grid-cols-[120px_1fr]">
              <div className="space-y-2">
                <Label htmlFor="key">Devnet key</Label>
                <Input id="key" type="number" min={0} max={59} value={index} disabled={busy} onChange={(e) => setIndex(Number(e.target.value))} />
              </div>
              <div className="truncate rounded-md border bg-muted/30 px-3 py-2 font-mono text-xs text-muted-foreground">{account?.account ?? "…"}</div>
            </div>
            <div className="flex gap-2">
              <Button onClick={start} disabled={!configured || !ready || busy || !account} className="flex-1">
                {busy ? <Loader2Icon className="animate-spin" /> : <FingerprintIcon />} Verify with World ID
              </Button>
              <Button variant="outline" disabled={!uri} onClick={() => abort.current?.abort()}>
                <XIcon /> Cancel
              </Button>
            </div>

            <ol className="space-y-2">
              {STAGES.map((s, i) => {
                const done = stage > i;
                const now = stage === i && busy;
                return (
                  <li key={s} className="flex items-center gap-3 text-sm">
                    <span
                      className={cn(
                        "flex size-6 items-center justify-center rounded-full border text-[11px]",
                        done && "border-ok/40 bg-ok/15 text-ok",
                        now && "border-primary/50 bg-primary/10 text-primary",
                      )}
                    >
                      {done ? <CheckIcon className="size-3.5" /> : now ? <Loader2Icon className="size-3.5 animate-spin" /> : i + 1}
                    </span>
                    <span className={cn(!done && !now && "text-muted-foreground")}>{s}</span>
                  </li>
                );
              })}
            </ol>

            {status && (
              <Alert variant={status.tone === "bad" ? "destructive" : "default"} className={cn(status.tone === "ok" && "border-ok/40", status.tone === "warn" && "border-warn/40")}>
                {status.tone === "ok" ? <ShieldCheckIcon className="text-ok" /> : <FingerprintIcon />}
                <AlertTitle>{status.title}</AlertTitle>
                <AlertDescription className="break-words">{status.text}</AlertDescription>
              </Alert>
            )}
            {log && <pre className="max-h-64 overflow-auto rounded-md border bg-black/40 p-3 font-mono text-[11px] whitespace-pre-wrap break-all">{log}</pre>}
          </CardContent>
        </Card>

        <div className="flex flex-col gap-4">
          <Card className="flex-1">
            <CardHeader>
              <CardTitle className="flex items-center gap-2">
                <ScanLineIcon className="size-4 text-primary" /> World App
              </CardTitle>
              <CardDescription>The request&apos;s QR code appears here while it waits for consent.</CardDescription>
            </CardHeader>
            <CardContent className="flex flex-col items-center gap-4">
              <div className={cn("relative flex size-64 items-center justify-center rounded-2xl border", uri ? "bg-white p-4 shadow-[0_0_60px] shadow-primary/20" : "border-dashed bg-muted/20")}>
                {uri ? <QRCodeSVG value={uri} size={224} level="M" /> : <FingerprintIcon className="size-14 text-muted-foreground/40" />}
              </div>
              {uri && (
                <Button variant="link" asChild>
                  <a href={uri} target="_blank" rel="noreferrer">
                    Open the request <ExternalLinkIcon />
                  </a>
                </Button>
              )}
            </CardContent>
          </Card>
          <Card>
            <CardHeader>
              <CardTitle>Why Proof of Human</CardTitle>
              <CardDescription>The map needs uniqueness and nothing else</CardDescription>
            </CardHeader>
            <CardContent className="space-y-2 text-sm">
              {[
                ["ok", "Proof of Human", "A unique, live human (Orb). Minimum sufficient, reveals nothing else."],
                ["bad", "Passport", "One person can hold several documents, and it would tie votes to document holders."],
                ["bad", "Selfie Check", "A risk signal, not a uniqueness verdict: too weak for votes that steer traffic and pay money."],
              ].map(([tone, t, d]) => (
                <div key={t} className="flex gap-3 rounded-lg border bg-muted/20 p-3">
                  {tone === "ok" ? <CheckIcon className="mt-0.5 size-4 shrink-0 text-ok" /> : <XIcon className="mt-0.5 size-4 shrink-0 text-bad" />}
                  <div>
                    <div className="font-medium">{t}</div>
                    <div className="text-xs text-muted-foreground">{d}</div>
                  </div>
                </div>
              ))}
              <p className="pt-1 text-xs text-muted-foreground">
                The control plane checks the action, the environment, its own nonce, the credential and the signal, then the Developer Portal checks the
                proof. The registry refuses any credential but Proof of Human and burns the nullifier, so a second key for the same human fails on chain.
              </p>
            </CardContent>
          </Card>
        </div>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Registrations</CardTitle>
          <CardDescription>The transactions that followed a verified proof</CardDescription>
        </CardHeader>
        <CardContent>
          <RecentJobs kinds={["worldid-attester", "worldid-auditor", "worldid-operator"]} limit={6} empty="No World ID registration yet." />
        </CardContent>
      </Card>
    </>
  );
}
