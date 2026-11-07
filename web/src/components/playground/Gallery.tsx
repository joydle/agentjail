import { useMemo, useState } from "react";
import { Panel } from "../Panel";
import { cn } from "../../lib/cn";
import { LangBadge, type Lang } from "../LangBadge";
import { RECIPES, GROUPS, type Recipe } from "../../lib/recipes";

const KNOWN_LANGS: ReadonlySet<Lang> = new Set(["rust", "ts", "js", "py", "sh"]);
const isLang = (s: string | undefined): s is Lang => !!s && KNOWN_LANGS.has(s as Lang);

export function Gallery({
  activeId,
  onPick,
}: {
  activeId: string;
  onPick: (id: string) => void;
}) {
  const [query, setQuery] = useState("");

  const byGroup = useMemo(() => {
    const needle = query.trim().toLowerCase();
    const keep = (r: Recipe) =>
      !needle ||
      r.title.toLowerCase().includes(needle) ||
      r.description.toLowerCase().includes(needle) ||
      (r.display?.toLowerCase().includes(needle) ?? false);

    const map = new Map<string, Recipe[]>();
    for (const r of RECIPES) {
      if (!keep(r)) continue;
      const arr = map.get(r.group) ?? [];
      arr.push(r);
      map.set(r.group, arr);
    }
    return map;
  }, [query]);

  const totalShown = Array.from(byGroup.values()).reduce((n, a) => n + a.length, 0);

  return (
    <Panel padded={false} className="overflow-hidden flex flex-col">
      <div className="px-4 py-4">
        <div className="flex items-center justify-between">
          <div className="text-[10px] uppercase tracking-[0.22em] text-ink-400 font-medium">
            Gallery
          </div>
          <span className="text-[10px] text-ink-600 mono">{totalShown}/{RECIPES.length}</span>
        </div>
        <div className="display text-base font-semibold mt-1 mb-3">Recipes & snippets</div>
        <SearchBox value={query} onChange={setQuery} />
      </div>
      <div className="hairline" />
      <nav className="flex-1 overflow-y-auto py-2">
        {GROUPS.map((g) => {
          const items = byGroup.get(g.id) ?? [];
          if (items.length === 0) return null;
          return (
            <div key={g.id} className="mb-3">
              <div className="flex items-center justify-between px-4 h-6 mt-1">
                <span className="text-[10px] uppercase tracking-[0.22em] text-ink-500 font-medium">
                  {g.label}
                </span>
                <span className="text-[10px] text-ink-600 mono">{items.length}</span>
              </div>
              {g.hint && (
                <div className="px-4 pb-1 text-[10.5px] leading-snug text-ink-500">
                  {g.hint}
                </div>
              )}
              <ul className="px-2 space-y-0.5">
                {items.map((r) => (
                  <li key={r.id}>
                    <RecipeButton recipe={r} on={r.id === activeId} onPick={onPick} />
                  </li>
                ))}
              </ul>
            </div>
          );
        })}
        {totalShown === 0 && (
          <div className="px-4 py-10 text-center text-xs text-ink-500 mono">
            nothing matches &ldquo;{query}&rdquo;
          </div>
        )}
      </nav>
    </Panel>
  );
}

function SearchBox({
  value,
  onChange,
}: {
  value: string;
  onChange: (v: string) => void;
}) {
  return (
    <div className="flex items-center gap-2 h-8 px-2.5 rounded-md bg-ink-900/70 ring-1 ring-ink-800 focus-within:ring-ink-600 transition">
      <span className="text-ink-500 text-[11px]">⌕</span>
      <input
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder="search recipes"
        className="flex-1 min-w-0 bg-transparent outline-none text-[12px] text-ink-200 placeholder:text-ink-600"
      />
      {value && (
        <button
          onClick={() => onChange("")}
          className="text-ink-500 hover:text-ink-200 text-[11px] leading-none"
          aria-label="clear search"
        >
          ×
        </button>
      )}
    </div>
  );
}

function RecipeButton({
  recipe,
  on,
  onPick,
}: {
  recipe: Recipe;
  on: boolean;
  onPick: (id: string) => void;
}) {
  return (
    <button
      onClick={() => onPick(recipe.id)}
      className={cn(
        "w-full text-left px-2.5 py-2 rounded-md transition-colors",
        on
          ? "bg-[var(--color-phantom-bg)] ring-1 ring-[var(--color-phantom)]/40"
          : "hover:bg-ink-850/60",
      )}
    >
      <div className="flex items-center justify-between gap-2">
        <span className={cn("text-[12.5px] truncate", on ? "text-ink-100" : "text-ink-200")}>
          {recipe.title}
        </span>
        {isLang(recipe.display) ? (
          <LangBadge lang={recipe.display} size={16} />
        ) : recipe.display ? (
          <span
            className={cn(
              "text-[9px] mono px-1.5 h-4 rounded tracking-wider",
              on
                ? "bg-[var(--color-phantom)]/20 text-[var(--color-phantom)]"
                : "bg-ink-800 text-ink-400",
            )}
          >
            {recipe.display}
          </span>
        ) : null}
      </div>
      <div
        className={cn(
          "text-[10.5px] mt-0.5 line-clamp-2 leading-snug",
          on ? "text-ink-400" : "text-ink-500",
        )}
      >
        {recipe.description}
      </div>
    </button>
  );
}
