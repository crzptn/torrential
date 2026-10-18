use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let libtorrent_src = manifest.join("../../vendor/libtorrent");
    if !libtorrent_src.join("deps/try_signal/try_signal.cpp").exists() {
        panic!("vendor/libtorrent is missing or incomplete; run `git submodule update --init --recursive`");
    }

    let boost_root = required_env("BOOST_ROOT");
    let openssl_include = required_env("OPENSSL_INCLUDE_DIR");
    let openssl_lib = required_env("OPENSSL_LIB_DIR");

    let install = cmake::Config::new(&libtorrent_src)
        .generator("Ninja")
        // libtorrent at -O0 makes swarm tests slow, so the cargo profile never applies.
        .profile("Release")
        .define("BUILD_SHARED_LIBS", "OFF")
        .define("CMAKE_INSTALL_LIBDIR", "lib")
        .define("encryption", "ON")
        .define("gnutls", "OFF")
        .define("deprecated-functions", "OFF")
        .define("webtorrent", "OFF")
        .define("Boost_ROOT", &boost_root)
        .define("OPENSSL_INCLUDE_DIR", &openssl_include)
        .define("OPENSSL_SSL_LIBRARY", Path::new(&openssl_lib).join("libssl.a"))
        .define("OPENSSL_CRYPTO_LIBRARY", Path::new(&openssl_lib).join("libcrypto.a"))
        .build();

    let defines = exported_defines(&install.join("lib/pkgconfig/libtorrent-rasterbar.pc"));
    for required in ["TORRENT_USE_OPENSSL", "TORRENT_ABI_VERSION=100", "TORRENT_USE_RTC=0"] {
        assert!(
            defines.iter().any(|d| d == required),
            "libtorrent was configured without {required}; exported defines: {defines:?}"
        );
    }

    let mut shim = cxx_build::bridge("src/bridge.rs");
    shim.file("cpp/shim.cpp")
        .std("c++17")
        .include(install.join("include"))
        .include(Path::new(&boost_root).join("include"))
        .include(&openssl_include);
    for define in &defines {
        match define.split_once('=') {
            Some((name, value)) => shim.define(name, value),
            None => shim.define(define, None),
        };
    }
    shim.compile("torrential-shim");

    println!("cargo:rustc-link-search=native={}", install.join("lib").display());
    println!("cargo:rustc-link-lib=static=torrent-rasterbar");
    println!("cargo:rustc-link-search=native={openssl_lib}");
    println!("cargo:rustc-link-lib=static=ssl");
    println!("cargo:rustc-link-lib=static=crypto");
    if cfg!(target_os = "macos") {
        println!("cargo:rustc-link-lib=framework=CoreFoundation");
        println!("cargo:rustc-link-lib=framework=SystemConfiguration");
    }

    for path in ["cpp/shim.h", "cpp/shim.cpp", "src/bridge.rs"] {
        println!("cargo:rerun-if-changed={path}");
    }
    for path in ["CMakeLists.txt", "include", "src"] {
        println!("cargo:rerun-if-changed={}", libtorrent_src.join(path).display());
    }
    for var in ["BOOST_ROOT", "OPENSSL_INCLUDE_DIR", "OPENSSL_LIB_DIR"] {
        println!("cargo:rerun-if-env-changed={var}");
    }
}

fn required_env(name: &str) -> String {
    env::var(name)
        .unwrap_or_else(|_| panic!("{name} is not set; run cargo inside `nix develop` from the workspace root"))
}

/// The `-D` flags from libtorrent's generated pkg-config file. Compiling the
/// shim with different definitions than the library breaks the ABI silently.
fn exported_defines(pc: &Path) -> Vec<String> {
    let text = fs::read_to_string(pc).unwrap_or_else(|e| panic!("reading {}: {e}", pc.display()));
    let cflags = text
        .lines()
        .find_map(|line| line.strip_prefix("Cflags:"))
        .unwrap_or_else(|| panic!("{} has no Cflags line", pc.display()));
    cflags.split_whitespace().filter_map(|flag| flag.strip_prefix("-D")).map(str::to_owned).collect()
}
