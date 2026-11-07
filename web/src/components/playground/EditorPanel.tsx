import type { ReactNode } from "react";
import { Panel } from "../Panel";
import { Pill } from "../Pill";
import { Button } from "../Button";
import { CodeEditor } from "../CodeEditor";
import { GROUPS, type Recipe } from "../../lib/recipes";

export function EditorPanel({
  recipe,
  code,
  onChange,
  onCopy,
  copied,
  runAction,
}: {
  recipe: Recipe;
  code: string;
  onChange: (next: string) => void;
  onCopy: () => void;
  copied: boolean;
  runAction: ReactNode;
}) {
  const group = GROUPS.find((g) => g.id === recipe.group);
  const runnable = recipe.kind === "run";

  return (
    <Panel padded={false} className="flex-1 flex flex-col overflow-hidden min-h-0">
      <header className="px-5 py-3 flex items-center justify-between border-b border-ink-800">
        <div className="min-w-0">
          <div className="text-[10px] uppercase tracking-[0.22em] text-ink-400 font-medium mb-0.5 flex items-center gap-2">
            <span>{group?.label}</span>
            <span className="text-ink-600">·</span>
            <span className="text-ink-500 truncate">{group?.hint}</span>
          </div>
          <h2 className="display text-base font-semibold truncate">{recipe.title}</h2>
        </div>
        <div className="flex items-center gap-2 shrink-0">
          <FileChip name={fileNameFor(recipe)} />
          <Button size="sm" variant="ghost" onClick={onCopy}>
            {copied ? "copied ✓" : "copy"}
          </Button>
          {runnable ? runAction : <Pill tone="ink">read-only</Pill>}
        </div>
      </header>
      <CodeEditor
        value={code}
        onChange={onChange}
        readOnly={!runnable}
        language={recipe.display ?? recipe.language}
      />
    </Panel>
  );
}

function FileChip({ name }: { name: string }) {
  return (
    <span className="hidden md:inline-flex items-center gap-1.5 h-6 px-2 text-[10.5px] mono rounded bg-ink-900/60 ring-1 ring-ink-800 text-ink-400">
      <span className="w-1 h-1 rounded-full bg-ink-500" />
      {name}
    </span>
  );
}

function fileNameFor(r: Recipe): string {
  if (r.language === "javascript") return "main.mjs";
  if (r.language === "python")     return "main.py";
  if (r.language === "bash")       return "main.sh";
  if (r.display === "ts")          return `${r.id}.ts`;
  if (r.display === "rust")        return `${r.id}.rs`;
  return r.id;
}
