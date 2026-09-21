import { DocPage, Section, Inline, Card, Cols, Hint } from "../../components/docs/DocPage";
import { LangBadge, type Lang } from "../../components/LangBadge";
import { RECIPES, GROUPS } from "../../lib/recipes";

const KNOWN: Record<string, Lang> = { ts: "ts", js: "js", py: "py", sh: "sh", rust: "rust" };

export function PlaygroundDoc() {
  return (
    <DocPage
      eyebrow="Build"
      title="Playground recipes"
      lead={
        <>
          Every recipe in the in-app playground, browsable and copyable here. The
          <Inline>Run</Inline> family hits <Inline>/v1/runs</Inline> directly, the{" "}
          <Inline>SDK</Inline> family wraps the same calls in{" "}
          <Inline>@agentjail/sdk</Inline>, and the <Inline>Advanced</Inline> family
          drops to the Rust library for primitives the HTTP API doesn't expose yet.
        </>
      }
    >
      <Hint title="Open the live editor" tone="phantom">
        Sign in and visit <a className="underline" href="/playground">Playground</a>{" "}
        to run any of these against your local stack with one click.
      </Hint>

      {GROUPS.map((g) => {
        const items = RECIPES.filter((r) => r.group === g.id);
        if (items.length === 0) return null;
        return (
          <Section key={g.id} id={g.id} title={g.label}>
            <p className="mb-3">{g.hint}</p>
            <Cols>
              {items.map((r) => (
                <Card key={r.id} title={r.title}>
                  <div className="flex items-start justify-between gap-2">
                    <span>{r.description}</span>
                    {r.display && KNOWN[r.display] && (
                      <LangBadge lang={KNOWN[r.display]} />
                    )}
                  </div>
                </Card>
              ))}
            </Cols>
          </Section>
        );
      })}
    </DocPage>
  );
}
