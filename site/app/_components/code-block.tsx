import { MarketingProofFrame, SyntaxCode } from "@hraness/design-kit/react/server";

export function Terminal({ code, title = "Terminal" }: { code: string; title?: string }) {
  return (
    <MarketingProofFrame title={title}>
      <pre tabIndex={0}><SyntaxCode code={code} language="shell" styles="classes" /></pre>
    </MarketingProofFrame>
  );
}
