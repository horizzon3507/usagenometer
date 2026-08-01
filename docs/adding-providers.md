# Adding a provider (CLI-first)

As of **0.1.3-beta**, the GNOME Shell extension is a thin client over `usg json`.
**You only add providers in Rust.** No new `providers/<name>/*.js` module is required.

## Checklist (Rust)

1. **`src/providers/<name>.rs`** — implement `fetch()` → `ProviderSnapshot` (and optional `test()`).
2. **`src/providers/mod.rs`** — `mod <name>;`, label, `fetch_one` / `test_provider` match arms.
3. **`src/cli.rs`** — add `ProviderArg::<Name>` (+ `id()` / `all()`).
4. **`src/config.rs`** — `parse_provider_name()` arm.
5. **`src/doctor.rs`** (optional but recommended) — auth-path check, no secrets.
6. **`src/explain.rs`** (optional) — meter/plan prose for `usg explain`.
7. **Docs** — README providers table + CHANGELOG under the CLI surface.

That’s the entire ship path for meters in both the CLI and the GNOME panel.

## GNOME

- Panel / prefs call `usg json -q -p …` and `usg test -q -p …`.
- Prefs catalog prefers `usg providers -q`; falls back to a static id list in `providers/types.js`.
- Enabled gschema IDs that match `^[a-z][a-z0-9_-]*$` are passed through even if not in the static catalog.
- Optionally bump the companion GNOME version / soft-catalog labels when you want nicer prefs copy for a brand-new id.

## Do not

- Do not reintroduce per-provider Soup/HTTP or auth parsers under `providers/` in JS.
- Do not store tokens; only read upstream CLI / app auth the same way other providers do.
