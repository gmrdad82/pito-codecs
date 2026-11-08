#!/usr/bin/env bash
set -euo pipefail
here=$(cd "$(dirname "$(readlink -f "$0")")" && pwd)
name=pcod
opt="${PCOD_OPT:-$HOME/.local/opt/pcod}"
copy="$opt/skills/$name"
homes=("$HOME/.agents/skills" "$HOME/.claude/skills")
[ -d "$HOME/.codex" ] && homes+=("$HOME/.codex/skills")
[ -d "$HOME/.pi" ] && homes+=("$HOME/.pi/agent/skills")
ours() {
  local dest=$1
  if [ -L "$dest" ]; then [ "$(readlink "$dest")" = "$copy" ]; return; fi
  [ -f "$dest/.pito-skill" ]
}
case "${1:-install}" in
  install)
    [ -f "$here/$name/SKILL.md" ] || { echo "install-skill: no $name skill beside this script" >&2; exit 1; }
    rm -rf -- "$opt/skills.new"
    mkdir -p "$opt/skills.new"
    cp -R "$here/$name" "$opt/skills.new/$name"
    rm -rf -- "$opt/skills"
    mv "$opt/skills.new" "$opt/skills"
    for home in "${homes[@]}"; do
      dest="$home/$name"
      if { [ -e "$dest" ] || [ -L "$dest" ]; } && ! ours "$dest"; then
        echo "install-skill: $dest is not ours, left alone"
        continue
      fi
      mkdir -p "$home"
      if [ -d "$dest" ] && [ ! -L "$dest" ]; then rm -rf -- "$dest"; fi
      ln -sfn "$copy" "$dest"
    done
    ;;
  uninstall)
    for home in "$HOME/.agents/skills" "$HOME/.claude/skills" "$HOME/.codex/skills" "$HOME/.pi/agent/skills"; do
      dest="$home/$name"
      if { [ -e "$dest" ] || [ -L "$dest" ]; } && ours "$dest"; then
        if [ -L "$dest" ]; then rm -f -- "$dest"; else rm -rf -- "$dest"; fi
      fi
    done
    rm -rf -- "$opt/skills"
    ;;
  *)
    echo "usage: install-skill.sh [install|uninstall]" >&2
    exit 2
    ;;
esac
