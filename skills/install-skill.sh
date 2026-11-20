#!/usr/bin/env bash
set -euo pipefail
here=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)
pcod=""
for c in "$here/../pcod" "$here/../bin/pcod"; do
  [ -x "$c" ] && { pcod=$c; break; }
done
[ -n "$pcod" ] || pcod=$(command -v pcod || true)
[ -n "$pcod" ] || { echo "install-skill: no pcod beside this folder or on PATH" >&2; exit 1; }
case "${1:-install}" in
  install | uninstall | status) exec "$pcod" skill "${1:-install}" ;;
  *)
    echo "usage: install-skill.sh [install|uninstall|status]" >&2
    exit 2
    ;;
esac
