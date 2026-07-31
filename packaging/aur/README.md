# AUR packaging (`usagenometer`)

Published: https://aur.archlinux.org/packages/usagenometer

## Install

```bash
yay -S usagenometer
# or
paru -S usagenometer
```

Also: `cargo install usagenometer` (crates.io).

## Tag scheme

See [VERSIONING.md](../../VERSIONING.md). Summary:

| Surface | Tag | Publishes |
|---------|-----|-----------|
| CLI | `cli/vX.Y.Z-alpha\|beta\|stable` | crates.io + AUR + GH Release |
| GNOME Shell | `gnome/vX.Y.Z-…` | companion / changelog |

Arch `pkgver` cannot contain `-`, so `0.1.1-beta` becomes `0.1.1_beta` in the PKGBUILD while the Git tag keeps the SemVer channel.

## Automatic publish

Every **`cli/v*` tag** (and manual **Actions → release**) runs [`.github/workflows/release.yml`](../../.github/workflows/release.yml):

1. Publishes crates.io (`CARGO_REGISTRY_TOKEN`)
2. Bumps `packaging/aur/PKGBUILD` + `.SRCINFO`
3. Pushes the package to the AUR (`AUR_SSH_PRIVATE_KEY`)

### One-time setup

```bash
gh secret set AUR_SSH_PRIVATE_KEY < ~/.ssh/aur_synara
gh secret set CARGO_REGISTRY_TOKEN  # from https://crates.io/settings/tokens
```

Public key must already be on the AUR account.

### Day-to-day

```bash
# bump Cargo.toml + CHANGELOG first
git tag -a cli/v0.1.1-beta -m "CLI 0.1.1-beta"
git push origin cli/v0.1.1-beta
# → Actions publishes crates.io + AUR
```

## Local publish (fallback)

```bash
./packaging/aur/publish.sh                 # push current packaging/
./packaging/aur/publish.sh cli/v0.1.1-beta # bump + push
```

Uses `~/aur/usagenometer` and `~/.ssh/aur_synara` (override with `AUR_SSH_KEY=` / `AUR_DIR=`).
