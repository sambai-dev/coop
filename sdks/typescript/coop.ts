/** Compatibility exports for the pre-Rookhold TypeScript API names. */

export * from "./rookhold.js";
export { Rookhold as Coop, RookholdError as CoopError } from "./rookhold.js";
export type {
  RookholdErrorInit as CoopErrorInit,
  RookholdEvent as CoopEvent,
  HashedRookholdEvent as HashedCoopEvent,
} from "./rookhold.js";
