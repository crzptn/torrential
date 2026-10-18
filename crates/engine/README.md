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

## Static release builds

OpenSSL links dynamically today, from `OPENSSL_LIB_DIR` in the dev shell. A
fully static release link needs:

- Static `libssl.a` and `libcrypto.a`, for example nixpkgs `pkgsStatic.openssl`
  or the `openssl-src` crate.
- `OPENSSL_USE_STATIC_LIBS=ON` and the `.a` paths in the CMake defines, and
  `static=ssl` and `static=crypto` link lines in `build.rs`.
- On Linux musl, a static C++ runtime (`-static-libstdc++`, or libc++ built
  for musl).
- A CA bundle that libtorrent's OpenSSL loads, because a static OpenSSL does
  not find the system trust store. The plan's spike 0c covers this.

macOS needs nothing more. libc++, libSystem, CoreFoundation, and
SystemConfiguration are always dynamic system libraries.
