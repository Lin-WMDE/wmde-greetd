#!/bin/sh
set -e
command -v systemctl >/dev/null 2>&1 && systemctl daemon-reload || true
