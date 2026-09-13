# Usage fixtures

These responses were captured from signed-in accounts on 13 September 2026. They contain usage metadata, with no credentials, account identifiers, prompts or conversation content.

`claude-oauth-usage.json` was captured at 19:15:58 UTC from Anthropic's OAuth usage endpoint with Claude Code 2.1.270 installed. It retains every response key, value type and quota value. The text of `spend.disclaimer` was replaced with `<redacted>` before saving. Whitespace was reformatted.

`codex-wham-usage.json` was captured at 19:06:53 UTC from OpenAI's usage endpoint with Codex CLI 0.154.0 installed. It retains the unmodified quota subset: `plan_type`, `rate_limit`, `code_review_rate_limit`, `additional_rate_limits`, `credits` and `rate_limit_reached_type`. Account fields and unrelated metadata were omitted before saving. This is not a complete wire-schema snapshot.

The matching `*-normalized.json` files describe the full expected output of the parsers. Review changes to them alongside the source response. Unknown fields remain visible as unavailable data. Identical percentages do not establish that two windows are the same limit. A weekly primary Codex window stays weekly. Credit balances without a denominator stay balances.

Synthetic edge cases live in the parser tests. Codex spend-control field names follow OpenAI's [account rate-limit tests](https://github.com/openai/codex/blob/main/codex-rs/app-server/tests/suite/v2/rate_limits.rs) and [backend mapping](https://github.com/openai/codex/blob/main/codex-rs/backend-client/src/client.rs). A fixed fixture test detects changes to our normalization, not future server changes by itself.
