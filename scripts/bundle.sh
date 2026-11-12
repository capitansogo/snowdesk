#!/bin/zsh
# Собирает Snowdesk.app в target/ и (с флагом --install) кладёт в /Applications.
# Подпись: сертификатом из scripts/make_cert.sh, если он создан, иначе ad-hoc
# (тогда после каждой пересборки доступ «Универсальный доступ» надо выдавать заново).
set -euo pipefail
cd "$(dirname "$0")/.."
[[ -f "$HOME/.cargo/env" ]] && source "$HOME/.cargo/env"

VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
APP=target/Snowdesk.app
ICONSET=target/AppIcon.iconset

# SNOWDESK_BIN — готовый бинарник (в CI это universal из lipo), иначе собираем сами.
BIN="${SNOWDESK_BIN:-target/release/snowdesk}"
if [[ -z "${SNOWDESK_BIN:-}" ]]; then
  cargo build --release
fi
cargo run -q --release --example make_icon -- target/icon_1024.png >/dev/null

rm -rf "$APP" "$ICONSET"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" "$ICONSET"
for s in 16 32 128 256 512; do
  sips -z $s $s target/icon_1024.png --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
  d=$((s * 2))
  sips -z $d $d target/icon_1024.png --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"
sed "s/__VERSION__/$VERSION/g" packaging/Info.plist > "$APP/Contents/Info.plist"
cp "$BIN" "$APP/Contents/MacOS/snowdesk"
# Постоянный сертификат (scripts/make_cert.sh), если есть, иначе ad-hoc.
SIGN="-"
if security find-identity -p codesigning 2>/dev/null | grep -q "Snowdesk Local Signing"; then
  SIGN="Snowdesk Local Signing"
fi
codesign --force --sign "$SIGN" --identifier app.snowdesk "$APP"
echo "подпись: ${SIGN/-/ad-hoc}"
echo "собрано: $APP ($(du -sh "$APP" | cut -f1))"

if [[ "${1:-}" == "--install" ]]; then
  pkill -x snowdesk 2>/dev/null || true
  rm -rf /Applications/Snowdesk.app
  cp -R "$APP" /Applications/
  echo "установлено: /Applications/Snowdesk.app"
  open /Applications/Snowdesk.app
fi
