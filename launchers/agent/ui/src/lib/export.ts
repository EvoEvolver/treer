import type { HistoryItem, Snapshot } from "../types";
import { turnError } from "./format";

const fence = (text: string, lang = "") => {
  const longest = Math.max(2, ...[...text.matchAll(/`+/g)].map((match) => match[0].length));
  const mark = "`".repeat(longest + 1);
  return `${mark}${lang}\n${text}\n${mark}`;
};
const FROM = /^\[来自 ([^\r\n]+)，排队于 ([^\r\n]+)\]\n/;

function itemMarkdown(item: HistoryItem): string {
  switch (item.kind) {
    case "userMessage": {
      const origin = FROM.exec(item.text);
      return `## ${origin ? `From ${origin[1]}` : "You"}\n\n${origin ? item.text.slice(origin[0].length) : item.text}`;
    }
    case "agentMessage": return item.text;
    case "reasoning": return `> Reasoning\n>\n${item.text.split("\n").map((line) => `> ${line}`).join("\n")}`;
    case "commandExecution": return `${fence(`$ ${item.text}`, "sh")}${item.status ? `\n_Status: ${item.status}_` : ""}`;
    case "fileChange":
      return (item.changes ?? []).map((change) => `**${change.kind} · ${change.path}**\n\n${fence(change.diff, "diff")}`).join("\n\n") || `File change: ${item.text}`;
    case "plan": return `**Plan**\n\n${(item.steps ?? []).map((step) => `- [${step.status === "completed" ? "x" : " "}] ${step.step}${step.status === "inProgress" ? " (in progress)" : ""}`).join("\n")}`;
    default: return `_${item.text || item.kind}_`;
  }
}

/** The loaded conversation as Markdown. Only what is already in the page is exported; nothing is fetched. */
export function toMarkdown(snapshot: Snapshot): string {
  const turns = snapshot.thread?.turns ?? [];
  const title = snapshot.thread?.title || snapshot.harness.name;
  const body = turns.flatMap((turn) => [...turn.items.map(itemMarkdown), ...(turn.error ? [`> Error: ${turnError(turn.error)}`] : [])]).filter(Boolean);
  return `# ${title}\n\n${body.join("\n\n")}\n`;
}

export function exportName(snapshot: Snapshot, now = new Date()): string {
  const base = (snapshot.thread?.title || snapshot.harness.name).toLowerCase().replace(/[^\p{L}\p{N}]+/gu, "-").replace(/^-+|-+$/g, "").slice(0, 60) || "conversation";
  return `${base}-${now.toISOString().slice(0, 10)}.md`;
}
