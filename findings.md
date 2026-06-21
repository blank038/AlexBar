# Findings

## Source Layout

- Frontend provider definitions live in `src/lib/types.ts`.
- Settings tabs and settings controls live in `src/SettingsApp.tsx`.
- Translations live in `src/lib/i18n.ts`.
- Backend provider registration lives in `src-tauri/src/providers.rs`.
- Usage implementations live under `src-tauri/src/usage/`.
- API key credential sources live under `src-tauri/src/credentials/`.

## Provider APIs

- Kimi balance endpoint returns `data.available_balance`, `data.voucher_balance`, and `data.cash_balance`, all in CNY.
- Minimax Token Plan remains endpoint returns `model_remains[]` with current interval and weekly usage/total/remaining percent fields.
- MiniMax CLI probes both `Authorization: Bearer <key>` and `x-api-key: <key>`; AlexBar will use Bearer first to match existing provider style.
