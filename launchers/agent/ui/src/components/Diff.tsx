import { memo } from "react";
import { t } from "../i18n";
import { parseDiff } from "../lib/diff";

/** One file's unified diff. File headers (---/+++) are dropped: the row above already names the file. */
export const Diff = memo(function Diff({ diff, label }: { diff: string; label: string }) {
  const lines = diff ? parseDiff(diff).filter((line) => line.kind !== "header") : [];
  return (
    <div className="diff" role="group" aria-label={t("diff.label", { path: label })} tabIndex={0}>
      {lines.length === 0 && <div className="dl"><span className="g" /><span className="c">{t("diff.none")}</span></div>}
      {lines.map((line, index) => (
        <div key={index} className={`dl ${line.kind}`}><span className="g">{line.gutter}</span><span className="c">{line.text}</span></div>
      ))}
    </div>
  );
});
