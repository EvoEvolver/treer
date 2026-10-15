import type { Principal } from "../types";

export function Avatar({ person }: { person?: Principal | null }) {
  return <span className={"avatar " + (person?.kind ?? "")} aria-hidden="true">{person ? person.name.slice(0, 2).toUpperCase() : "–"}</span>;
}
