# Versioning

This project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html) with an explicit **release channel** suffix, and [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## Surfaces + tags

| Surface | What it is | Git tag |
|---------|------------|---------|
| **CLI** | `usagenometer` / `usg` (Rust) — primary | `cli/vX.Y.Z-<channel>` |
| **GNOME Shell** | GNOME Shell extension (companion) | `gnome/vX.Y.Z-<channel>` |

Surfaces are versioned **independently**. Changelog headings name the surface, e.g. `## [CLI 0.1.1-beta]` or `## [GNOME Shell 0.1.0-beta]`.

**CLI** tags (`cli/v*`) publish to crates.io + AUR + GitHub Releases. **GNOME Shell** tags are companion / changelog artifacts unless explicitly promoted.

## Release channels (`x.y.z-<channel>`)

| Channel | Tag example | Meaning |
|---------|-------------|---------|
| **alpha** | `cli/v0.1.0-alpha` | Extremely early. Features incomplete; bugs are expected and common. |
| **beta** | `cli/v0.1.1-beta` | Feature set nearly complete, but still rough — bugs and hard edges remain. |
| **stable** | `cli/v0.2.0-stable` | Production-ready: finished for that version, few or no known bugs. |

Do **not** label something `stable` unless it is actually release-ready. Prefer **beta** while a surface is still maturing; use **alpha** only for brand-new / half-built work.

## Arch `pkgver` note

Arch forbids hyphens in `pkgver`, so `0.1.1-beta` becomes `0.1.1_beta` in the PKGBUILD while the Git tag keeps the SemVer channel (`cli/v0.1.1-beta`).
