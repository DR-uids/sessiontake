import type { ProjectSource } from "./projects";

export const tools = {
  codex: "Codex",
  harness: "DeepSeek Harness",
  claude: "Claude Code",
  cursor: "Cursor",
  workbuddy: "WorkBuddy",
} as const;

export type Source = keyof typeof tools;
export type Session = ProjectSource & {
  key: string;
  id: string;
  source: Source;
  origin: "codex" | "harness" | "claudeCli" | "claudeDesktop" | "cursor" | "workbuddy";
  title: string;
  updatedAt: number;
};

type Timed = { time: string | null };
export type ToolRow = Timed & {
  kind: "tool";
  index: number;
  name: string | null;
  hint: string | null;
  callId: string | null;
};
export type Row =
  | (Timed & {
    kind: "message";
    role: "user" | "assistant";
    text: TextSection[];
    index: number;
    continued: boolean;
    images?: ImageCard[];
  })
  | ToolRow;

export type ImageCard = { name: string | null; mime: string | null };
export type Format = "md" | "jsonl";
export type Page<T> = { content: T; total: number };
export type ToolPart = { index: number; command: boolean; text: string };

export function originName(session: Session): string {
  if (session.origin === "claudeCli") return "CLI";
  if (session.origin === "claudeDesktop") return "Desktop";
  return "";
}

export function exportFilename(title: string, format: Format, now = new Date()) {
  const date = [
    now.getFullYear(),
    String(now.getMonth() + 1).padStart(2, "0"),
    String(now.getDate()).padStart(2, "0"),
  ].join("-");
  const stem = title.replace(/[\/\\:*?"<>|\u0000-\u001f\u007f]/g, "-")
    .replace(/\s+/g, " ").trim().replace(/^\.+|\.+$/g, "").trim();
  // File names are limited by UTF-8 byte length on some volumes.
  const encoder = new TextEncoder();
  let name = "";
  let bytes = 0;
  for (const letter of stem) {
    bytes += encoder.encode(letter).length;
    if (bytes > 220) break;
    name += letter;
  }
  return `${date}-${name.trimEnd() || "session"}.${format}`;
}

export type TextSection = { text: string; block?: number };
export type CodeBlock =
  | { kind: "markdown"; block: number }
  | { kind: "message"; index: number; block: number }
  | { kind: "tool"; index: number };
export type SearchCursor = { entry: number; offset: number };
export type SearchHit = {
  kind: "source" | "user" | "assistant" | "command" | "output";
  before: string;
  matched: string;
  after: string;
};
