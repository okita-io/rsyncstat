# Packaging RSyncStat

## Local Arch / Omarchy install

From a git checkout (Rust 1.95+ on `PATH`):

```bash
cd pkgbuild && makepkg -fsi
```

That uses `pkgbuild/PKGBUILD`, which builds the parent tree.

## AUR (`rsyncstat`)

`v0.1.0` is tagged and public. `pkgbuild/PKGBUILD.aur` already has the release
tarball checksum. A ready-to-push clone lives at `~/aur/rsyncstat` (PKGBUILD +
`.SRCINFO`).

**Blocked (2026-11-14):** AUR new-account registration is temporarily closed
(HTTP 503). An SSH key was generated at `~/.ssh/aur` / `~/.ssh/aur.pub` for
when registration reopens.

When you have an AUR account:

1. Paste `~/.ssh/aur.pub` into AUR → My Account → SSH Public Key.
2. Push the prepared package:

```bash
cd ~/aur/rsyncstat
ssh -T aur@aur.archlinux.org   # should greet you by username
git push -u origin master
```

If the remote already has history (deleted package reuse), clone instead and
copy `PKGBUILD` / `.SRCINFO` into that tree.

To rebuild checksums for a later tag:

```bash
VER=0.1.0
curl -sL -o /tmp/rsyncstat-$VER.tar.gz \
  "https://github.com/okita-io/rsyncstat/archive/refs/tags/v$VER.tar.gz"
sha256sum /tmp/rsyncstat-$VER.tar.gz
```

Install for users (after the AUR push):

```bash
yay -S rsyncstat
# or
paru -S rsyncstat
```

## Omarchy packages

Draft PR: [omacom/omarchy-pkgs#2792](https://github.com/omacom/omarchy-pkgs/pull/2792)
(`pkgbuilds/rsyncstat`, same `v0.1.0` tarball and checksum as the AUR recipe).

Non-draft PRs that only touch `pkgbuilds/` auto-merge and publish to the `edge`
channel when CI is green. Mark #2792 ready when you want that.

After it lands on edge / stable, ask for install-list or migration wiring if
you want it on new Omarchy installs by default.

## Config file location

Named include/exclude packs live at:

- Linux: `~/.config/rsyncstat/include-exclude.yaml`
- Windows: `%APPDATA%\rsyncstat\include-exclude.yaml`

An existing `include-exclude.yaml` beside a portable binary is still used.
