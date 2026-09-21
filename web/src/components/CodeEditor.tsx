import { useMemo } from "react";
import CodeMirror, { EditorView, type Extension } from "@uiw/react-codemirror";
import { javascript } from "@codemirror/lang-javascript";
import { python } from "@codemirror/lang-python";
import { rust } from "@codemirror/lang-rust";
import { oneDark } from "@codemirror/theme-one-dark";
import { cn } from "../lib/cn";

/**
 * CodeMirror 6 editor tuned to the phantom palette. Read-only mode renders
 * the same view without a cursor, so SDK/advanced snippets keep the same
 * typography as executable recipes.
 */
export function CodeEditor({
  value,
  onChange,
  readOnly = false,
  language = "javascript",
  className,
}: {
  value: string;
  onChange?: (next: string) => void;
  readOnly?: boolean;
  language?: string;
  className?: string;
}) {
  const extensions = useMemo<Extension[]>(() => {
    const langExt = langFor(language);
    return [
      ...(langExt ? [langExt] : []),
      EditorView.lineWrapping,
      EditorView.theme({
        "&":            { height: "100%", background: "transparent" },
        ".cm-scroller": {
          fontFamily: "var(--font-mono)",
          fontSize: "12.5px",
          lineHeight: "1.6",
          letterSpacing: "-0.01em",
        },
        ".cm-gutters": {
          background: "transparent",
          borderRight: "1px solid var(--color-ink-800)",
          color: "var(--color-ink-600)",
        },
        ".cm-activeLineGutter, .cm-activeLine": { background: "transparent" },
        ".cm-content":  { padding: "14px 0", caretColor: "var(--color-phantom)" },
        ".cm-line":     { padding: "0 14px" },
        "&.cm-focused": { outline: "none" },
        ".cm-selectionBackground, ::selection": {
          background: "color-mix(in oklab, var(--color-phantom) 25%, transparent) !important",
        },
      }, { dark: true }),
    ];
  }, [language]);

  return (
    <div className={cn("flex-1 min-h-0 overflow-hidden", className)}>
      <CodeMirror
        value={value}
        onChange={(v) => onChange?.(v)}
        readOnly={readOnly}
        theme={oneDark}
        extensions={extensions}
        basicSetup={{
          lineNumbers: true,
          foldGutter: false,
          highlightActiveLine: false,
          highlightActiveLineGutter: false,
          dropCursor: true,
          indentOnInput: true,
          bracketMatching: true,
          closeBrackets: true,
          autocompletion: false,
        }}
        height="100%"
        style={{ height: "100%" }}
      />
    </div>
  );
}

function langFor(l: string): Extension | null {
  switch (l) {
    case "javascript": case "js":
      return javascript();
    case "typescript": case "ts":
      return javascript({ typescript: true });
    case "python": case "py":
      return python();
    case "rust":
      return rust();
    case "bash": case "sh":
      // bash highlighter isn't bundled by default; fall back to no lang.
      return null;
    default:
      return null;
  }
}
