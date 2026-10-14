#!/bin/sh
# Создаём системного пользователя через sysusers, перечитываем юниты.
# Сервис НЕ включаем: display manager выбирается вручную. Ошибки глушим.
set -e
command -v systemd-sysusers >/dev/null 2>&1 && systemd-sysusers /usr/lib/sysusers.d/greetd.conf || true
command -v systemctl >/dev/null 2>&1 && systemctl daemon-reload || true
