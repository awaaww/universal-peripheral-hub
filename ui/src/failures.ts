//! Human sentences for hub failures.
//!
//! `HubFailure.kind` is a stable machine tag; the panel pairs a plain-language
//! sentence with that tag so a user can act on it and a bug report still carries
//! the exact reason. Unknown kinds fall back to a generic sentence but keep the
//! tag.

import { t } from "./i18n";
import type { HubFailure } from "./protocol";

const KNOWN_KINDS = new Set([
  "unknown_device",
  "unknown_capability",
  "read_only",
  "invalid_value",
  "busy",
  "timeout",
  "transient",
  "unsupported",
  "permission_denied",
  "io",
  "protocol",
  "internal",
]);

/** `<sentence> (<kind>)`, or the bare generic sentence when there is no kind. */
export function failureText(failure: HubFailure): string {
  const kind = failure.kind ?? null;
  const sentence = kind && KNOWN_KINDS.has(kind) ? t(`failure.${kind}`) : t("failure.other");
  return kind ? `${sentence} (${kind})` : sentence;
}
