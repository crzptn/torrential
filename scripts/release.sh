#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
mkdir -p dist

case "$(uname -s) $(uname -m)" in
  "Darwin arm64")
    target=aarch64-apple-darwin
    nix --extra-experimental-features 'nix-command flakes' develop -c cargo build --release --locked -p torrential
    bin=target/release/torrential
    ;;
  "Linux x86_64")
    target=x86_64-unknown-linux-musl
    docker run --rm -v "$PWD:/src" -w /src -e OWNER="$(id -u):$(id -g)" alpine:3.24 sh -euc '
      apk add --no-cache build-base cmake samurai boost-dev openssl-dev openssl-libs-static linux-headers curl >/dev/null
      curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal >/dev/null 2>&1
      . /root/.cargo/env
      BOOST_ROOT=/usr OPENSSL_INCLUDE_DIR=/usr/include OPENSSL_LIB_DIR=/usr/lib \
        cargo build --release --locked -p torrential --target-dir target/linux
      chown -R "$OWNER" target/linux
    '
    bin=target/linux/release/torrential
    ;;
  *)
    echo "release.sh builds on macOS arm64 or Linux x86_64, not $(uname -s) $(uname -m)" >&2
    exit 1
    ;;
esac

asset=torrential-$target.tar.gz
stage=$(mktemp -d)
cp "$bin" LICENSE "$stage/"
tar -czf "dist/$asset" -C "$stage" torrential LICENSE
rm -rf "$stage"

cd dist
if command -v sha256sum >/dev/null 2>&1; then
  sha256sum "$asset" > "$asset.sha256"
else
  shasum -a 256 "$asset" > "$asset.sha256"
fi
cat "$asset.sha256"
