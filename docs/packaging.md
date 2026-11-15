# Packaging RSyncStat

## Local Arch / Omarchy install

From a git checkout (Rust 1.95+ on `PATH`):

```bash
cd pkgbuild && makepkg -fsi
```

That uses `pkgbuild/PKGBUILD`, which builds the parent tree.

## AUR (`rsyncstat`)

1. Tag and push a release (`v0.1.0`, …). GitHub Actions attaches platform archives.
2. Checksum the source tarball:

```bash
VER=0.1.0
curl -sL -o /tmp/rsyncstat-$VER.tar.gz \
  "https://github.com/okita-io/rsyncstat/archive/refs/tags/v$VER.tar.gz"
sha256sum /tmp/rsyncstat-$VER.tar.gz
```

3. Copy `pkgbuild/PKGBUILD.aur` into a fresh AUR clone and set `sha256sums`:

```bash
git -c init.defaultBranch=master clone ssh://aur@aur.archlinux.org/rsyncstat.git ~/aur/rsyncstat
cp pkgbuild/PKGBUILD.aur ~/aur/rsyncstat/PKGBUILD
# edit sha256sums, then:
cd ~/aur/rsyncstat
makepkg -f
namcap PKGBUILD *.pkg.tar.zst
makepkg --printsrcinfo > .SRCINFO
git add PKGBUILD .SRCINFO
git commit -m "rsyncstat $VER-1"
git push
```

AUR account + SSH key for `aur.archlinux.org` are required (see the [AUR submission guidelines](https://wiki.archlinux.org/title/AUR_submission_guidelines)).

Install for users:

```bash
yay -S rsyncstat
# or
paru -S rsyncstat
```

## Omarchy packages (after AUR)

Once tagged releases exist on GitHub:

1. Open a PR on [`omacom/omarchy-pkgs`](https://github.com/omacom/omarchy-pkgs) under `pkgbuilds/rsyncstat/`.
2. Build from the same release tarball as the AUR package (not a local checkout).
3. Add `.omarchy/package.json` with `"source": "local"`, `"auto_merge": true`, and an `upstream.watch` on `okita-io/rsyncstat`.

## Config file location

Named include/exclude packs live at:

- Linux: `~/.config/rsyncstat/include-exclude.yaml`
- Windows: `%APPDATA%\rsyncstat\include-exclude.yaml`

An existing `include-exclude.yaml` beside a portable binary is still used.
