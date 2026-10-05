/**
 * Ledger search box — monospace input with `/` focus shortcut and
 * inline clear. Used by any list page that wants server-side search.
 */
export function SearchBox({
  value,
  onChange,
  placeholder = "search",
  className = "",
}: {
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  className?: string;
}) {
  return (
    <div
      className={`flex items-center gap-2 h-8 px-2.5 rounded-md bg-ink-900/70 ring-1 ring-ink-800 focus-within:ring-ink-600 transition min-w-[280px] ${className}`}
      title="press / to focus"
    >
      <span className="text-ink-500 text-[11px]" aria-hidden>⌕</span>
      <input
        data-search-input
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={placeholder}
        aria-label={placeholder}
        className="flex-1 bg-transparent outline-none text-[12px] text-ink-200 placeholder:text-ink-600"
      />
      {value && (
        <button
          onClick={() => onChange("")}
          className="text-ink-500 hover:text-ink-200 text-[11px] leading-none"
          aria-label="clear search"
        >×</button>
      )}
    </div>
  );
}
