#!/bin/zsh
# Один раз создаёт самоподписанный сертификат «Snowdesk Local Signing» в связке ключей входа.
# Зачем: с ad-hoc подписью macOS после каждой пересборки считает приложение новым
# и доступ «Универсальный доступ» (снег на Dock) надо выдавать заново. С постоянным
# сертификатом доступ переживает обновления.
# Удалить: «Связка ключей» → вход → «Snowdesk Local Signing» (сертификат и ключ).
set -euo pipefail
NAME="Snowdesk Local Signing"
if security find-identity -p codesigning | grep -q "$NAME"; then
  echo "сертификат уже есть"; exit 0
fi
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
cat > "$TMP/cert.cnf" <<CNF
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = $NAME
[ext]
keyUsage = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
basicConstraints = critical, CA:false
CNF
openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -config "$TMP/cert.cnf" \
  -keyout "$TMP/key.pem" -out "$TMP/cert.pem" 2>/dev/null
PASS=$(openssl rand -hex 12)
openssl pkcs12 -export -legacy -inkey "$TMP/key.pem" -in "$TMP/cert.pem" -name "$NAME" \
  -out "$TMP/id.p12" -passout "pass:$PASS" 2>/dev/null \
  || openssl pkcs12 -export -inkey "$TMP/key.pem" -in "$TMP/cert.pem" -name "$NAME" \
       -out "$TMP/id.p12" -passout "pass:$PASS"
security import "$TMP/id.p12" -k "$HOME/Library/Keychains/login.keychain-db" -P "$PASS" -T /usr/bin/codesign
# доверие для подписи кода — macOS спросит пароль
security add-trusted-cert -p codeSign -k "$HOME/Library/Keychains/login.keychain-db" "$TMP/cert.pem"
security find-identity -p codesigning | grep "$NAME"
echo "готово: теперь ./scripts/bundle.sh --install подписывает этим сертификатом"
