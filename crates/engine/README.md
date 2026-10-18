# torrential-engine

The only crate in torrential that touches libtorrent. `build.rs` builds
`vendor/libtorrent` (v2.1.2) with CMake and Ninja in Release mode, links it
statically, and compiles the cxx shim in `cpp/` against the definitions
libtorrent exports.

Build and test from the workspace root, inside the Nix dev shell:

```sh
git submodule update --init --recursive
nix --extra-experimental-features 'nix-command flakes' develop -c cargo test -p torrential-engine
```

## Static linking

`build.rs` links libtorrent and OpenSSL statically. OpenSSL comes from
`libssl.a` and `libcrypto.a` in `OPENSSL_LIB_DIR`, which the dev shell points
at nixpkgs `openssl.override { static = true; }`. On macOS the binary loads
only libraries under `/usr/lib` and `/System/Library`, and
`crates/torrential/tests/linkage.rs` fails if that changes.

For HTTPS trackers, libtorrent also loads a CA bundle from fixed system paths:
`/etc/ssl/cert.pem` on macOS, and the distribution paths on Linux. A static
OpenSSL needs no CA bundle of its own. See
`vendor/libtorrent/src/session_impl.cpp`.

A fully static Linux musl build also needs a static C++ runtime
(`-static-libstdc++`, or libc++ built for musl).
