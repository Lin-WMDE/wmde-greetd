# Maintainer: WMDE <https://wmde.fun>
#
# Builds our own Lin-WMDE/wmde-greetd (branch wmde): greetd with one addition, a handover
# from the greeter to the session without the console showing in between
# ([terminal] session_vt = "next", see greetd/src/context.rs). Upstream is
# https://git.sr.ht/~kennylevinsen/greetd, mirrored by `master` and merged into `wmde`.
#
# Stands in for Arch's greetd: same binary, same unit, same PAM service, same system
# user. agreety and fakegreet are not shipped - the stack's greeter is wmde-greeter.
pkgname=wmde-greetd
pkgver=0.1.0
pkgrel=1
pkgdesc="WMDE build of greetd, the login manager daemon, with a seamless session handover"
arch=('x86_64')
url="https://wmde.fun"
license=('GPL-3.0-only')
depends=('glibc' 'gcc-libs' 'pam' 'systemd')
makedepends=('rust' 'cargo' 'git')
provides=('greetd')
conflicts=('greetd')
backup=('etc/greetd/config.toml' 'etc/pam.d/greetd')
source=("$pkgname::git+https://github.com/Lin-WMDE/wmde-greetd.git#branch=wmde")
sha256sums=('SKIP')

pkgver() {
  cd "$srcdir/$pkgname"
  # WMDE version: <COSMIC major.minor>.<WMDE release>.<commits since the nearest tag>.g<short>.
  local desc
  desc=$(git describe --long --tags --abbrev=7 2>/dev/null || true)
  if [ -n "$desc" ]; then
    printf '1.10.0.%s.g%s' "$(printf '%s' "$desc" | sed -E 's/.*-([0-9]+)-g[0-9a-f]+$/\1/')" "$(git rev-parse --short=7 HEAD)"
  else
    printf '1.10.0.%s.g%s' "$(git rev-list --count HEAD)" "$(git rev-parse --short=7 HEAD)"
  fi
}

build() {
  cd "$srcdir/$pkgname"
  # x86-64-v3 (AVX2/BMI2) baseline for the WMDE repo; runs on Haswell+ (and the VM).
  export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-C target-cpu=x86-64-v3"
  cargo build --release --locked -p greetd
}

check() {
  cd "$srcdir/$pkgname"
  # The config parser carries the new option; the handover itself needs a seat and logind.
  cargo test --release --locked -p greetd
}

package() {
  cd "$srcdir/$pkgname"
  install -Dm755 "${CARGO_TARGET_DIR:-target}/release/greetd" "$pkgdir/usr/bin/greetd"
  install -Dm644 config.toml "$pkgdir/etc/greetd/config.toml"
  install -Dm644 packaging/greetd.pam "$pkgdir/etc/pam.d/greetd"
  install -Dm644 greetd.service "$pkgdir/usr/lib/systemd/system/greetd.service"
  install -Dm644 packaging/greetd.sysusers "$pkgdir/usr/lib/sysusers.d/greetd.conf"
  install -Dm644 LICENSE "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
}
