import { useMemo, useState } from "react";
import CodeMirror, { EditorView, type Extension } from "@uiw/react-codemirror";
import { javascript } from "@codemirror/lang-javascript";
import { python } from "@codemirror/lang-python";
import { rust } from "@codemirror/lang-rust";
import { oneDark } from "@codemirror/theme-one-dark";
import { StreamLanguage } from "@codemirror/language";
import { shell } from "@codemirror/legacy-modes/mode/shell";
import { json } from "@codemirror/legacy-modes/mode/javascript";
import { yaml } from "@codemirror/legacy-modes/mode/yaml";
import { toml } from "@codemirror/legacy-modes/mode/toml";
import { cn } from "../../lib/cn";

const LABEL: Record<string, string> = {
  ts: "typescript", tsx: "typescript",
  js: "javascript", jsx: "javascript",
  py: "python", python: "python",
  bash: "bash", sh: "shell",
  rust: "rust",
  json: "json", yaml: "yaml", toml: "toml",
  text: "text",
};

/**
 * Static, read-only code block for the docs. Reuses the same CodeMirror /
 * one-dark stack as the Playground so highlighting feels consistent across
 * the dashboard.
 */
export function Code({
  lang = "bash",
  children,
  filename,
}: {
  lang?: string;
  children: string;
  filename?: string;
}) {
  const [copied, setCopied] = useState(false);
  const value = stripTrailingNewline(children);

  async function copy() {
    try {
      await navigator.clipboard.writeText(value);
      setCopied(true);
      setTimeout(() => setCopied(false), 1200);
    } catch {
      /* ignored */
    }
  }

  const extensions = useMemo<Extension[]>(() => {
    const langExt = langFor(lang);
    return [
      ...(langExt ? [langExt] : []),
      EditorView.lineWrapping,
      EditorView.editable.of(false),
      EditorView.theme({
        "&":            { background: "transparent" },
        ".cm-scroller": {
          fontFamily: "var(--font-mono)",
          fontSize:   "12.5px",
          lineHeight: "1.6",
          letterSpacing: "-0.01em",
        },
        ".cm-content":  { padding: "12px 0", caretColor: "transparent" },
        ".cm-line":     { padding: "0 16px" },
        ".cm-cursor":   { display: "none" },
        ".cm-activeLine, .cm-activeLineGutter": { background: "transparent" },
        "&.cm-focused": { outline: "none" },
      }, { dark: true }),
    ];
  }, [lang]);

  const label = filename ?? LABEL[lang] ?? lang;
  return (
    <div className="my-4 panel !p-0 overflow-hidden">
      <div className="flex items-center justify-between px-3 py-1.5 border-b border-ink-700/60 bg-ink-900/60">
        <span className="text-[10px] uppercase tracking-[0.18em] text-ink-500 mono">
          {label}
        </span>
        <button
          onClick={copy}
          className={cn(
            "text-[10px] mono px-2 py-0.5 rounded transition-colors",
            copied
              ? "text-[var(--color-phantom)]"
              : "text-ink-500 hover:text-ink-200",
          )}
          aria-label="copy code"
        >
          {copied ? "copied ✓" : "copy"}
        </button>
      </div>
      <CodeMirror
        value={value}
        readOnly
        editable={false}
        theme={oneDark}
        extensions={extensions}
        basicSetup={{
          lineNumbers: false,
          foldGutter: false,
          highlightActiveLine: false,
          highlightActiveLineGutter: false,
          dropCursor: false,
          indentOnInput: false,
          bracketMatching: false,
          closeBrackets: false,
          autocompletion: false,
          highlightSelectionMatches: false,
        }}
      />
    </div>
  );
}

function langFor(l: string): Extension | null {
  switch (l) {
    case "javascript": case "js": case "jsx":
      return javascript();
    case "typescript": case "ts": case "tsx":
      return javascript({ typescript: true });
    case "python": case "py":
      return python();
    case "rust":
      return rust();
    case "bash": case "sh":
      return StreamLanguage.define(shell);
    case "json":
      return StreamLanguage.define(json);
    case "yaml":
      return StreamLanguage.define(yaml);
    case "toml":
      return StreamLanguage.define(toml);
    default:
      return null;
  }
}

function stripTrailingNewline(s: string): string {
  return s.endsWith("\n") ? s.slice(0, -1) : s;
}
