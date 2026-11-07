import { useMutation } from "@tanstack/react-query";
import { useState } from "react";
import { useApi } from "../lib/auth";
import type { ExecResult } from "../lib/api";
import { Button } from "../components/Button";
import { Gallery } from "../components/playground/Gallery";
import { EditorPanel } from "../components/playground/EditorPanel";
import { Terminal } from "../components/playground/Terminal";
import { RECIPES } from "../lib/recipes";

/**
 * Playground page = a composition:
 *   [Gallery]  │   [EditorPanel]
 *              │   [Terminal    ]
 */
export function Playground() {
  const api = useApi();
  const [recipeId, setRecipeId] = useState(RECIPES[0].id);
  const recipe = RECIPES.find((r) => r.id === recipeId) ?? RECIPES[0];
  const [code, setCode] = useState(recipe.code);
  const [copied, setCopied] = useState(false);
  const [drawerOpen, setDrawerOpen] = useState(true);

  function pick(id: string) {
    const r = RECIPES.find((x) => x.id === id);
    if (!r) return;
    setRecipeId(id);
    setCode(r.code);
    setCopied(false);
  }

  const run = useMutation<ExecResult>({
    mutationFn: () =>
      api.runs.create(code, recipe.language ?? "javascript", recipe.timeoutSecs),
    onSuccess: () => setDrawerOpen(true),
  });

  async function copy() {
    await navigator.clipboard.writeText(code);
    setCopied(true);
    setTimeout(() => setCopied(false), 1200);
  }

  return (
    <div
      className="grid gap-4"
      style={{ gridTemplateColumns: "260px minmax(0,1fr)", height: "calc(100vh - 100px)" }}
    >
      <Gallery activeId={recipeId} onPick={pick} />
      <div className="flex flex-col gap-4 min-h-0">
        <EditorPanel
          recipe={recipe}
          code={code}
          onChange={setCode}
          onCopy={copy}
          copied={copied}
          runAction={
            <Button
              variant="primary"
              size="sm"
              disabled={run.isPending || !code.trim()}
              onClick={() => run.mutate()}
            >
              {run.isPending ? "running…" : "run ▸"}
            </Button>
          }
        />
        <Terminal
          open={drawerOpen}
          onToggle={() => setDrawerOpen((v) => !v)}
          recipe={recipe}
          result={run.data}
          error={run.error}
          pending={run.isPending}
        />
      </div>
    </div>
  );
}
