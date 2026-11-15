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

## Omarchy packages (after AUR)

Once the AUR package is live (or in parallel, using the same tarball):

1. Fork/clone [`omacom/omarchy-pkgs`](https://github.com/omacom/omarchy-pkgs).
2. Add `pkgbuilds/rsyncstat/` with a PKGBUILD matching `pkgbuild/PKGBUILD.aur`
   (same `source` URL and `sha256sums`).
3. Add `.omarchy/package.json`:

```json
{
  "source": "local",
  "auto_merge": true,
  "upstream": {
    "watch": "https://github.com/okita-io/rsyncstat"
  }
}
```

4. Open a PR. After merge, ask for install-list / migration wiring if you want
   it on new Omarchy installs by default.

## Config file location

Named include/exclude packs live at:

- Linux: `~/.config/rsyncstat/include-exclude.yaml`
- Windows: `%APPDATA%\rsyncstat\include-exclude.yaml`

An existing `include-exclude.yaml` beside a portable binary is still used.
