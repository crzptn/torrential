//! An installed binary has to start after `nix-collect-garbage` and on a Mac
//! without Nix, so it may load only libraries that macOS ships.
#![cfg(target_os = "macos")]

use std::process::Command;

#[test]
fn binary_loads_only_system_libraries() {
    let out = Command::new("otool").arg("-L").arg(env!("CARGO_BIN_EXE_torrential")).output().unwrap();
    assert!(out.status.success(), "otool -L failed: {}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).unwrap();
    let (system, foreign): (Vec<&str>, Vec<&str>) = text
        .lines()
        .skip(1)
        .filter_map(|line| line.split_whitespace().next())
        .partition(|path| path.starts_with("/usr/lib/") || path.starts_with("/System/Library/"));
    assert!(system.contains(&"/usr/lib/libSystem.B.dylib"), "otool -L output:\n{text}");
    assert_eq!(foreign, Vec::<&str>::new());
}
