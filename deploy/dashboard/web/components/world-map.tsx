"use client";

import "leaflet/dist/leaflet.css";
import { useEffect, useRef } from "react";
import { latLngBounds, type CircleMarker as LeafletCircleMarker } from "leaflet";
import { CircleMarker, MapContainer, Pane, Polygon, Popup, TileLayer, Tooltip, useMap } from "react-leaflet";
import { useTheme } from "next-themes";
import { bestMs, latencyTone, type Cell, type DedanNode, type Human } from "@/lib/api";
import { LogSheet, TerminalLink } from "@/components/log-sheet";

export type Selection = { cell?: string; node?: string; client?: string };

/** A human's device sits at its declared "lat,lon". */
export const clientAt = (h: Human): [number, number] => h.location.split(",").map(Number) as [number, number];

/** Frames the nodes once, when they first arrive. */
function FitNodes({ nodes }: { nodes: DedanNode[] }) {
  const map = useMap();
  const done = useRef(false);
  useEffect(() => {
    const pts = nodes.filter((n) => n.location).map((n) => [n.location!.lat, n.location!.lon] as [number, number]);
    if (done.current || !pts.length) return;
    done.current = true;
    map.fitBounds(latLngBounds(pts), { padding: [60, 60], maxZoom: 5 });
  }, [map, nodes]);
  return null;
}

/** A marker whose selected look is a class toggled on its SVG path: rebuilding the layer would close its popup. */
function Marker({ chosen, chosenClass, ...props }: React.ComponentProps<typeof CircleMarker> & { chosen: boolean; chosenClass: string }) {
  const ref = useRef<LeafletCircleMarker>(null);
  useEffect(() => {
    ref.current?.getElement()?.classList.toggle(chosenClass, chosen);
  }, [chosen, chosenClass]);
  return <CircleMarker ref={ref} {...props} />;
}

/** What a click on a marker opens: the container's terminal and logs. */
function MarkerPopup({ title, detail, container }: { title: string; detail: string; container: string }) {
  return (
    <Popup closeButton={false} offset={[0, -6]}>
      <div className="min-w-48 space-y-2">
        <div>
          <div className="font-mono text-sm font-medium">{title}</div>
          <div className="text-xs text-muted-foreground">{detail}</div>
        </div>
        <div className="flex gap-1">
          <TerminalLink name={container} />
          <LogSheet name={container} />
        </div>
      </div>
    </Popup>
  );
}

/** Cells (resolution-2 hexagons some node joined) coloured by their best median, the clients (rings) and the
 *  nodes (dots) on top. A click selects and opens the container's actions. */
export default function WorldMap({
  cells,
  nodes,
  humans,
  selection,
  onSelect,
}: {
  cells: Cell[];
  nodes: DedanNode[];
  humans: Human[];
  selection: Selection;
  onSelect: (s: Selection) => void;
}) {
  const { resolvedTheme } = useTheme();
  const style = resolvedTheme === "light" ? "World_Light_Gray_Base" : "World_Dark_Gray_Base";
  return (
    <MapContainer center={[30, 20]} zoom={2} minZoom={2} worldCopyJump className="size-full" attributionControl>
      <TileLayer
        key={style}
        url={`https://server.arcgisonline.com/ArcGIS/rest/services/Canvas/${style}/MapServer/tile/{z}/{y}/{x}`}
        attribution="Tiles &copy; Esri &mdash; Esri, HERE, Garmin, &copy; OpenStreetMap contributors"
        maxZoom={16}
      />
      <FitNodes nodes={nodes} />
      {/* fixed stacking: a cell rebuilt on selection must not land on top of the markers and take their clicks */}
      <Pane name="cells" style={{ zIndex: 390 }}>
        {cells
          .filter((c) => c.boundary?.length)
          .map((c) => {
            const best = bestMs(c);
            const chosen = selection.cell === c.cell || (!!selection.node && c.members.includes(selection.node));
            // Leaflet reads className only when it creates the path: a new class means a new layer (the key)
            const className = `cell cell-${latencyTone(best)}${chosen ? " cell-chosen" : ""}`;
            return (
              <Polygon
                key={`${c.cell} ${className}`}
                className={className}
                positions={c.boundary}
                pathOptions={{
                  weight: chosen ? 2.5 : 1,
                  fillOpacity: best === null ? 0.12 : 0.35,
                }}
                eventHandlers={{ click: () => onSelect({ cell: c.cell }) }}
              >
                <Tooltip sticky>
                  <div className="text-xs">
                    <div className="font-mono font-medium">{c.name}</div>
                    <div className="text-muted-foreground">
                      {c.members.join(", ")} · {best === null ? "no votes yet" : `${c.nodes[0].label} ${best} ms`}
                    </div>
                  </div>
                </Tooltip>
              </Polygon>
            );
          })}
      </Pane>
      <Pane name="clients" style={{ zIndex: 440 }}>
        {humans.map((h) => {
          const chosen = selection.client === h.city;
          return (
            <Marker
              key={h.city}
              chosen={chosen}
              chosenClass="client-chosen"
              className="client-marker"
              center={clientAt(h)}
              radius={chosen ? 13 : 11}
              pathOptions={{ weight: 2.5, fillOpacity: 0.12 }}
              eventHandlers={{ click: () => onSelect({ client: h.city }) }}
            >
              <Tooltip direction="bottom" offset={[0, 10]}>
                <div className="text-xs">
                  <div className="font-mono font-medium">user-{h.city}</div>
                  <div className="text-muted-foreground">
                    {h.kind === "client" ? "client" : "city user"} · {h.ip} · human key {h.attester}
                  </div>
                </div>
              </Tooltip>
              <MarkerPopup
                title={`user-${h.city}`}
                detail={`${h.kind === "client" ? "client device" : "city user"} · ${h.ip} · human key ${h.attester}`}
                container={`user-${h.city}`}
              />
            </Marker>
          );
        })}
      </Pane>
      <Pane name="nodes" style={{ zIndex: 450 }}>
        {nodes
          .filter((n) => n.location)
          .map((n) => (
            <Marker
              key={n.label}
              chosen={selection.node === n.label}
              chosenClass="node-chosen"
              className="node-marker"
              center={[n.location!.lat, n.location!.lon]}
              radius={selection.node === n.label ? 7 : 5}
              pathOptions={{ weight: 2, fillOpacity: 1 }}
              eventHandlers={{ click: () => onSelect({ node: n.label }) }}
            >
              <Tooltip direction="top" offset={[0, -8]}>
                <div className="text-xs">
                  <div className="font-mono font-medium">{n.name}</div>
                  <div className="text-muted-foreground">
                    {n.ipv4 ?? "no IPv4"} · {n.class} · {n.up === false ? "no heartbeat" : "up"}
                  </div>
                </div>
              </Tooltip>
              <MarkerPopup
                title={n.name}
                detail={`CDN node · ${n.ipv4 ?? "no IPv4"} · ${n.class} · ${n.up === false ? "no heartbeat" : "up"}`}
                container={n.label}
              />
            </Marker>
          ))}
      </Pane>
    </MapContainer>
  );
}
