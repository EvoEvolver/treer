import { Markdown } from "../Markdown";
export default function MarkdownPreview({ content }: { content: string }) {
  return <div className="file-markdown"><Markdown text={content} /></div>;
}
