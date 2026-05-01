#!/usr/bin/env bash
set -euo pipefail

if ! command -v gpg &>/dev/null; then
    echo "gpg not found — installing via Homebrew..."
    brew install gnupg
fi

gpg --batch --gen-key <<EOF
Key-Type: RSA
Key-Length: 4096
Name-Real: llmproxy release
Name-Email: tomu.hirata@gmail.com
Expire-Date: 0
%no-protection
EOF

KEY_FILE=$(mktemp)
trap 'rm -f "$KEY_FILE"' EXIT

gpg --armor --export-secret-keys tomu.hirata@gmail.com > "$KEY_FILE"

gh secret set APT_GPG_PRIVATE_KEY --repo TomeHirata/llm-proxy < "$KEY_FILE"
echo -n "" | gh secret set APT_GPG_PASSPHRASE --repo TomeHirata/llm-proxy

echo "Done — APT_GPG_PRIVATE_KEY and APT_GPG_PASSPHRASE set on TomeHirata/llm-proxy"
