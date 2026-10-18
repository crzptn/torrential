mod bridge;

pub fn libtorrent_version() -> String {
    bridge::ffi::libtorrent_version().unwrap()
}

#[cfg(test)]
mod tests {
    #[test]
    fn links() {
        assert_eq!(super::libtorrent_version(), "2.1.2.0");
    }
}
