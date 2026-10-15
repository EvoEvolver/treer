import type { ReactNode } from "react";
import { words } from "../lib";

export function Field({ text, children }: { text: string; children: ReactNode }) {
  return <label>{text}{children}</label>;
}

// The picker holds its own buttons and inputs, so it is grouped with a div rather than wrapped by a label.
export function PickerField({ text, children }: { text: string; children: ReactNode }) {
  return <div className="field"><span>{text}</span>{children}</div>;
}

export function Select<T extends string>({ text, values, value, onChange, disabled }: {
  text: string; values: readonly T[]; value: T; onChange: (value: T) => void; disabled?: boolean;
}) {
  return (
    <Field text={text}>
      <select aria-label={text} value={value} disabled={disabled} onChange={(e) => onChange(e.target.value as T)}>
        {values.map((v) => <option key={v} value={v}>{words(v)}</option>)}
      </select>
    </Field>
  );
}
