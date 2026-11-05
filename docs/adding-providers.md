# Adding a provider

**You only add providers in Rust.**

## Checklist (Rust)

1. **`src/providers/<name>.rs`** — implement `fetch()` → `ProviderSnapshot` (and optional `test()`).
2. **`src/providers/mod.rs`** — `mod <name>;`, label, `fetch_one` / `test_provider` match arms.
3. **`src/cli.rs`**: add `ProviderArg::<Name>` with its exact clap value name, `id()` and `catalog()` entry; add to `all()` only for a status surface.
4. **`src/config.rs`** — `parse_provider_name()` arm.
5. **`src/doctor.rs`** (optional but recommended) — auth-path check, no secrets.
6. **`src/explain.rs`** (optional) — meter/plan prose for `usg explain`.
7. **Docs** — README providers table + CHANGELOG under the CLI surface.
8. **`src/providers/availability.rs`**: detect a real installation and inspect the provider's native credential source/precedence without API calls, secret output or credential commands. Uninspectable storage is `unknown`, not a verified login.

That’s the entire ship path for meters and ledger scanners.

## Do not

- Do not store tokens; only read upstream CLI / app auth the same way other providers do.
