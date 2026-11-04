# Versioning

This project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html) with an explicit **release channel** suffix. Public changelog headings follow the Hory style: a leading `v`, a `DD/MM/YYYY` date, and one human summary sentence.

## Surface + tags

| Surface | What it is | Git tag |
|---------|------------|---------|
| **CLI** | `usagenometer` / `usg` (Rust) | `cli/vX.Y.Z-<channel>` |

There is one installable surface, so release tags carry no `m` and release notes do not split into per-surface sections.

The **GNOME Shell** companion shipped through `v0.1.5m-beta` and is discontinued as of `v0.1.6-beta`. Historical `gnome/v*` tags remain for that surface only; no new GNOME builds are produced.

**CLI** tags (`cli/v*`) publish to crates.io + AUR + GitHub Releases.

## Release channels (`x.y.z-<channel>`)

| Channel | Tag example | Meaning |
|---------|-------------|---------|
| **alpha** | `cli/v0.1.0-alpha` | Extremely early. Features incomplete; bugs are expected and common. |
| **beta** | `cli/v0.1.1-beta` | Feature set nearly complete, but still rough — bugs and hard edges remain. |
| **stable** | `cli/v0.2.0-stable` | Production-ready: finished for that version, few or no known bugs. |

Do **not** label something `stable` unless it is actually release-ready. Prefer **beta** while the CLI is still maturing; use **alpha** only for brand-new / half-built work.

## Arch `pkgver` note

Arch forbids hyphens in `pkgver`, so `0.1.1-beta` becomes `0.1.1_beta` in the PKGBUILD while the Git tag keeps the SemVer channel (`cli/v0.1.1-beta`).
