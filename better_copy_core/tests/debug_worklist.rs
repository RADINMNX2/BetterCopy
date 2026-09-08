#![cfg(windows)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use better_copy_core::walker::{build_work_list, ensure_long_path};

#[test]
fn debug_build_work_list_stats() {
    let root = std::env::temp_dir().join(format!("bc_debug_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let src = root.join("src");
    let dest = root.join("dest");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dest).unwrap();
    fs::create_dir_all(&src.join("dir00")).unwrap();
    for f in 0..3 {
        fs::write(src.join("dir00").join(format!("file_{f}.bin")), vec![7u8; 1024]).unwrap();
    }

    let src_long = ensure_long_path(&src);
    let cancel = Arc::new(AtomicBool::new(false));

    println!("--- path diagnostics ---");
    println!("raw src: {}", src.display());
    println!("long src: {}", src_long.display());
    println!("raw_temp: {}", std::env::temp_dir().display());

    match fs::symlink_metadata(&src_long) {
        Ok(m) => println!("long metadata OK is_dir={} attr={:x}", m.is_dir(), m.file_attributes()),
        Err(e) => println!("long metadata ERR: {e}"),
    }
    match fs::symlink_metadata(&src) {
        Ok(m) => println!("raw metadata OK is_dir={} attr={:x}", m.is_dir(), m.file_attributes()),
        Err(e) => println!("raw metadata ERR: {e}"),
    }
    match fs::read_dir(&src_long) {
        Ok(rd) => {
            let n = rd.count();
            println!("read_dir(long) entries = {n}");
        }
        Err(e) => println!("read_dir(long) ERR: {e}"),
    }
    match fs::read_dir(&src) {
        Ok(rd) => {
            let n = rd.count();
            println!("read_dir(raw) entries = {n}");
        }
        Err(e) => println!("read_dir(raw) ERR: {e}"),
    }

    match build_work_list(&[src.clone()], &dest, Some(&cancel), None) {
        Ok(wl) => println!(
            "work list OK dirs={} small={} large={} total_files={} skipped={}",
            wl.dirs.len(),
            wl.small_files.len(),
            wl.large_files.len(),
            wl.total_files,
            wl.skipped_links.len()
        ),
        Err(e) => println!("work list ERR: {e}"),
    }

    let _ = fs::remove_dir_all(&root);
}