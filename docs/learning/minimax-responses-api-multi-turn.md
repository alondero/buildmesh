---
name: minimax-responses-api-multi-turn
description: MiniMax's OpenAI Responses endpoint rejects previous_response_id with HTTP 500; multi-turn must replay explicit input items
metadata:
  type: reference
---

# MiniMax Responses API — Multi-Turn Contract (verified live, 2026-08)

Findings from debugging the codex:minimax pairing verification (provider_verification.rs), each confirmed against `https://api.minimax.io/v1/responses` with a real key:

## The trap: `previous_response_id` → HTTP 500

MiniMax's Responses implementation does **not** support `previous_response_id`. It is absent from their published schema (`platform.minimax.io/docs/api-reference/responses-create`), and sending it returns **HTTP 500 with an empty body** — not a 400/404. A request that works on OpenAI can therefore hard-fail here.

**Correct multi-turn shape** (their documented pattern, also valid OpenAI): replay history as explicit input items:

```json
{
  "model": "MiniMax-M3",
  "input": [
    { "type": "message", "role": "user", "content": "..." },
    { "type": "function_call", "call_id": "...", "name": "...", "arguments": "{...}" },
    { "type": "function_call_output", "call_id": "...", "output": "..." }
  ],
  "stream": true
}
```

Other verified facts:
- `"strict": true` on function tools **is accepted** (echoed back in `response.created`).
- Streaming SSE includes `event:` lines plus `data:` lines; `output_item.added` may carry a **partial** `arguments` string — only trust `response.function_call_arguments.done`.
- Auth is checked **before routing**: an unauthenticated probe returns 401 for *any* path, so you cannot use a bad-key 401 to prove an endpoint exists. A valid-auth unknown/mis-shaped request surfaces as 500.
- Model IDs drift (`MiniMax-M3[1m]` was retired); Buildmesh pins `MiniMax-M3` for Codex pairings and capability-checks anything else (`endpoint_model_descriptor`).

## Debugging lesson

The failure never showed its real cause in the UI: any signature mismatch (including a routine `codex-cli` npm auto-update) was re-labelled "routing inputs changed", masking the stored HTTP 500 reason. When a status layer derives messages from *change classification*, always test what happens when the underlying record already carries a truthful failure reason. Regression-pinned by `stale_after_cli_update_reports_the_cli_change_not_routing` and `incompatible_capability_record_keeps_its_reason_instead_of_stale_mask`.
