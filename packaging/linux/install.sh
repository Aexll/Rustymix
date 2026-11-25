#!/bin/sh
# Ajoute Rustymix au menu des applications de l'utilisateur (sans droits
# administrateur) : programme dans ~/.local/bin, icône et raccourci dans
# ~/.local/share. Relancer ce script met l'installation à jour.
set -e
cd "$(dirname "$0")"
BIN="$HOME/.local/bin"
ICONS="$HOME/.local/share/icons/hicolor/256x256/apps"
APPS="$HOME/.local/share/applications"
mkdir -p "$BIN" "$ICONS" "$APPS"
install -m 755 rustymix "$BIN/rustymix"
install -m 644 rustymix.png "$ICONS/rustymix.png"
sed "s|^Exec=.*|Exec=$BIN/rustymix|" rustymix.desktop > "$APPS/rustymix.desktop"
chmod 644 "$APPS/rustymix.desktop"
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$APPS" || true
echo "Rustymix est installé : cherchez-le dans le menu des applications."
