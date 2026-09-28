import { GobFigure } from "./gob-figure";

const BRANCHES = [
  { kind: "pass", text: "under the threshold: sent unchanged" },
  { kind: "rewrite", text: "over the threshold: rewritten" },
  { kind: "fallback", text: "anything goes wrong: your original request is sent" },
] as const;

/** D-route: where the proxy sits and what it does with each request. */
export function GobRoute() {
  return (
    <GobFigure
      alt="Diagram: Claude Code sends to gobstopper proxy on 127.0.0.1 port 8260, which sends to the model provider. Small requests pass unchanged, large ones are rewritten, and on any error the original request is sent."
      caption="If a rewrite fails or the provider rejects it for any reason other than length, Gobstopper sends the original bytes. A length rejection gets one more trim and a retry."
      id="route"
      kind="diagram"
      title="It sits between your agent and the provider"
    >
      <div className="gob-route">
        <ol className="gob-route__pills">
          <li className="gob-route__pill">Claude Code</li>
          <li className="gob-route__pill" data-proxy="">
            gobstopper proxy <span aria-hidden="true">·</span> <code>127.0.0.1:8260</code>
          </li>
          <li className="gob-route__pill">model provider</li>
        </ol>
        <ul className="gob-route__branches">
          {BRANCHES.map((branch) => <li data-kind={branch.kind} key={branch.kind}>{branch.text}</li>)}
        </ul>
        <p className="gob-route__foot">Logs record sizes and counts, never request text. The proxy never edits your saved session.</p>
      </div>
    </GobFigure>
  );
}
