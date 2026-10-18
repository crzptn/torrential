#[cxx::bridge(namespace = "torrential")]
pub(crate) mod ffi {
    unsafe extern "C++" {
        include!("torrential-engine/cpp/shim.h");
        fn libtorrent_version() -> Result<String>;
    }
}
