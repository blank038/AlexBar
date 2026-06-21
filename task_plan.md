# Task Plan

## Goal

Add Minimax and Kimi usage/balance support, improve Chinese Provider wording, and split settings into General, Appearance, Provider, and System categories.

## Phases

1. Planning records - complete
2. Backend providers - complete
3. Frontend settings and translations - complete
4. README support table - complete
5. Tests and Tauri packaging - complete

## Decisions

- Kimi uses the official Moonshot/Kimi balance endpoint: `GET https://api.moonshot.cn/v1/users/me/balance`.
- Minimax uses the official Token Plan remains endpoint used by `mmx quota`: `GET https://api.minimaxi.com/v1/token_plan/remains`.
- Keep changes minimal and follow existing one-provider-per-file style instead of introducing a generic API key credential source.

## Errors Encountered

| Error | Attempt | Resolution |
| --- | --- | --- |
| `usage::minimax::tests::parses_minimax_remains_payload` expected `Tense` at 80% used | `cargo test` attempt 1 | Adjusted the fixture to 95% used so it matches the existing `urgency_from_percent` 90% threshold. |
