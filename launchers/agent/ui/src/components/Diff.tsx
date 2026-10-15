import { memo } from "react";
import { t } from "../i18n";
import { parseDiff } from "../lib/diff";
import { Icon } from "./Icon";

export const Diff = memo(function Diff({ change }: { change: { path: string; diff: string; kind: string } }) {
  const lines = change.diff ? parseDiff(change.diff) : [];
  return (
    <div className="diff" role="group" aria-label={change.path}>
      <div className="dh"><Icon name="file" /><span>{change.kind} · {change.path}</span></div>
      {lines.length === 0 && <div className="dl"><span className="g" /><span className="c">{t("diff.none")}</span></div>}
      {lines.map((line, index) => (
        <div key={index} className={`dl ${line.kind}`}><span className="g">{line.gutter}</span><span className="c">{line.text}</span></div>
      ))}
    </div>
  );
});
