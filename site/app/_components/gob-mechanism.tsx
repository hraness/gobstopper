import { DiagramArrowhead, MarketingDiagram } from "@hraness/design-kit/react/server";

/**
 * The mechanism in one drawing: the agent talks to Gobstopper on localhost,
 * Gobstopper talks to the model API, and the full history lands in a local
 * vault the agent can search. One accent, both appearances, no script.
 */

const W = 880;
const H = 330;
const NODE_H = 72;
const ROW_Y = 60;
const ARROW = "gob-mechanism-arrow";

type Node = Readonly<{ id: string; x: number; w: number; label: string; detail: string }>;

const AGENT: Node = { id: "agent", x: 20, w: 220, label: "Your agent", detail: "Claude Code, Codex, and more" };
const PROXY: Node = { id: "proxy", x: 330, w: 220, label: "Gobstopper proxy", detail: "on 127.0.0.1, no model call" };
const API: Node = { id: "api", x: 640, w: 220, label: "Model API", detail: "Anthropic, OpenAI, a gateway" };
const VAULT: Node = { id: "vault", x: 330, w: 220, label: "Local vault", detail: "the full history, on your disk" };
const VAULT_Y = 230;

function Box({ node, y, accent = false }: Readonly<{ node: Node; y: number; accent?: boolean }>) {
  return (
    <g>
      <rect className={accent ? "gob-mech__node gob-mech__node--accent" : "gob-mech__node"} height={NODE_H} rx="10" width={node.w} x={node.x} y={y} />
      <text className="gob-mech__label" x={node.x + node.w / 2} y={y + 30} textAnchor="middle">{node.label}</text>
      <text className="gob-mech__detail" x={node.x + node.w / 2} y={y + 52} textAnchor="middle">{node.detail}</text>
    </g>
  );
}

export function GobMechanismDiagram() {
  const mid = ROW_Y + NODE_H / 2;
  return (
    <MarketingDiagram
      caption="Requests leave your agent as usual. Past the threshold, Gobstopper rewrites the request before it reaches the model, and every turn it summarized stays readable in the vault."
      className="gob-mech"
      height={H}
      label="Diagram: your agent sends requests to the Gobstopper proxy on localhost, which forwards compacted requests to the model API and writes the full history to a local vault your agent can search and restore from."
      width={W}
    >
      <defs>
        <DiagramArrowhead id={ARROW} />
      </defs>
      <Box node={AGENT} y={ROW_Y} />
      <Box node={PROXY} y={ROW_Y} accent />
      <Box node={API} y={ROW_Y} />
      <Box node={VAULT} y={VAULT_Y} />

      {/* agent -> proxy */}
      <line className="gob-mech__edge" markerEnd={`url(#${ARROW})`} x1={AGENT.x + AGENT.w} x2={PROXY.x - 2} y1={mid - 10} y2={mid - 10} />
      <text className="gob-mech__edge-label" textAnchor="middle" x={(AGENT.x + AGENT.w + PROXY.x) / 2} y={ROW_Y - 12}>every request</text>
      {/* proxy -> agent (responses) */}
      <line className="gob-mech__edge gob-mech__edge--quiet" markerEnd={`url(#${ARROW})`} x1={PROXY.x} x2={AGENT.x + AGENT.w + 2} y1={mid + 14} y2={mid + 14} />
      <text className="gob-mech__edge-label" textAnchor="middle" x={(AGENT.x + AGENT.w + PROXY.x) / 2} y={mid + 32}>responses</text>

      {/* proxy -> api */}
      <line className="gob-mech__edge" markerEnd={`url(#${ARROW})`} x1={PROXY.x + PROXY.w} x2={API.x - 2} y1={mid - 10} y2={mid - 10} />
      <text className="gob-mech__edge-label" textAnchor="middle" x={(PROXY.x + PROXY.w + API.x) / 2} y={ROW_Y - 12}>compacted request</text>
      <line className="gob-mech__edge gob-mech__edge--quiet" markerEnd={`url(#${ARROW})`} x1={API.x} x2={PROXY.x + PROXY.w + 2} y1={mid + 14} y2={mid + 14} />
      <text className="gob-mech__edge-label" textAnchor="middle" x={(PROXY.x + PROXY.w + API.x) / 2} y={mid + 32}>responses</text>

      {/* proxy -> vault */}
      <line className="gob-mech__edge" markerEnd={`url(#${ARROW})`} x1={PROXY.x + PROXY.w / 2 + 30} x2={PROXY.x + PROXY.w / 2 + 30} y1={ROW_Y + NODE_H} y2={VAULT_Y - 2} />
      <text className="gob-mech__edge-label" x={PROXY.x + PROXY.w / 2 + 40} y={(ROW_Y + NODE_H + VAULT_Y) / 2 + 5}>full history</text>

      {/* vault -> agent: search and undo */}
      <path className="gob-mech__edge gob-mech__edge--quiet" d={`M${VAULT.x} ${VAULT_Y + NODE_H / 2} H${AGENT.x + AGENT.w / 2} V${ROW_Y + NODE_H + 2}`} fill="none" markerEnd={`url(#${ARROW})`} />
      <text className="gob-mech__edge-label" textAnchor="middle" x={(AGENT.x + AGENT.w / 2 + VAULT.x) / 2 + 10} y={VAULT_Y + NODE_H / 2 - 8}>search, undo</text>
    </MarketingDiagram>
  );
}
