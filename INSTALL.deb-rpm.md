# Установка на deb/rpm-дистрибутивах без сборки из сырцов вручную

Шаблон: см. предыдущие компоненты. Автор собирает пакеты у себя локально, как сейчас `makepkg -si`.

Опорные платформы — Debian stable и Fedora: `.deb` собираем на Debian (встаёт и на Ubuntu/Mint),
`.rpm` — на Fedora (встаёт и на RHEL/openSUSE). Наоборот не гарантируется.

## Собрать

```sh
# зависимости сборки: rustc>=1.93 cargo pam systemd + nfpm
cargo install --locked nfpm  # или бинарник с github.com/goreleaser/nfpm/releases

just deb   # -> target/wmde-greetd_*.deb (только -p greetd, как в PKGBUILD)
just rpm   # -> target/wmde-greetd-*.rpm
```

Версия — `1.10.0.<commits>.g<sha>`, как `pkgver()` в `PKGBUILD`.
По умолчанию сборка под чистый `x86-64`. Опционально под `x86-64-v3` (как в Arch-репозитории):

```sh
WMDE_CPU=x86-64-v3 just deb
```

Соседних чек-аутов не нужно. `justfile` в апстриме не было — добавлен минимальный
(сборка/установка/вендор + пакаж-рецепты), повторяет `build()`/`package()` из PKGBUILD.

## Офлайн-сборка (без сети к git-зависимостям)

`vendor.tar` в git не лежит (см. `.gitignore`), генерим на машине с сетью один раз:

```sh
just vendor          # -> vendor.tar (реестр + git-крейты)
just deb-vendored    # сборка --frozen --offline -> target/*.deb
just rpm-vendored    # аналогично -> target/*.rpm
```

На машину без сети привозим `vendor.tar` файлом.

## Поставить

```sh
# Debian/Ubuntu/Mint
sudo apt install ./target/wmde-greetd_*.deb

# Fedora/RHEL/openSUSE
sudo dnf install ./target/wmde-greetd-*.rpm
```

Пакет заменяет штатный `greetd` (provides/conflicts), конфиги — conffiles.
Сервис НЕ включается автоматически: display manager выбирается вручную.
Системный пользователь создаётся через sysusers в postinstall.
