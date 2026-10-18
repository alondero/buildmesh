// Module-scope type alias for the triage-deck chip action enum
// (issue #1377). Hoisted so `AttentionCard`'s `sent?: SentAction` prop
// doesn't have to repeat the literal union, and so the `useState<Map<...>>`
// calls in NodeList can avoid the `.tsx` JSX-vs-generic ambiguity that
// comes with back-to-back `<Map<...>>(...)` expressions.
export type SentAction = "approve" | "reject";

/**
 * A delivered action, bound to the request it answered (issue #1966).
 *
 * The node id alone is not enough: a node stays `awaiting_input` across
 * request boundaries, so a second question on the same node would inherit
 * the first one's disabled chips after a reconnect. Carrying the request key
 * means the recorded action only applies to the request it was sent for.
 */
export type SentMarker = { requestKey: string; action: SentAction };
