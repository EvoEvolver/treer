import type { HistoryItem } from "../types";

export type Block = { type: "message"; item: HistoryItem } | { type: "plan"; item: HistoryItem } | { type: "group"; items: HistoryItem[] };

/** Messages and plans stay in the conversation; every run of machine activity folds into one group. */
export function toBlocks(items: HistoryItem[]): Block[] {
  const blocks: Block[] = [];
  for (const item of items) {
    if (item.kind === "userMessage" || item.kind === "agentMessage") blocks.push({ type: "message", item });
    else if (item.kind === "plan") blocks.push({ type: "plan", item });
    else {
      const last = blocks.at(-1);
      if (last?.type === "group") last.items.push(item); else blocks.push({ type: "group", items: [item] });
    }
  }
  return blocks;
}

