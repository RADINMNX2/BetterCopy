#![cfg(windows)]

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use better_copy_core::engine::{run_engine, EngineSummary};
use better_copy_core::profiler::profile_device;
use better_copy_core::walker::build_work_list;

fn write_tree(root: &PathBuf, files: usize, dirs: usize, size: usize) -> PathBuf {
    fs::create_dir_all(root).unwrap();
    let blob: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
    for d in 0..dirs {
        let dir_path = root.join(format!("dir{d:02}"));
        fs::create_dir_all(&dir_path).unwrap();
        for f in 0..files {
            let needle = format!("{d:02}_{f:03}");
            let data = [blob.as_slice(), needle.as_bytes()].concat();
            fs::write(dir_path.join(format!("file_{needle}.bin")), &data).unwrap();
        }
    }
    root.to_path_buf()
}

fn list_tree(root: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(p: &std::path::Path, out: &mut Vec<String>) {
        if let Ok(rd) = fs::read_dir(p) {
            for e in rd.flatten() {
                out.push(e.path().display().to_string());
                if e.path().is_dir() {
                    walk(&e.path(), out);
                }
            }
        }
    }
    walk(root, &mut out);
    out
}

#[test]
fn debug_full_pipeline() {
    let root = std::env::temp_dir().join(format!("bc_debug2_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let src = write_tree(&root.join("src"), 5, 3, 1024 * 4 + 7);
    let dest = root.join("dest");
    fs::create_dir_all(&dest).unwrap();

    println!("=== profile_device(dest) ===");
    println!("{:?}", profile_device(&dest));

    let cancel = Arc::new(AtomicBool::new(false));
    let pause = Arc::new(AtomicBool::new(false));

    println!("=== build_work_list direct ===");
    match build_work_list(&[src.clone()], &dest, Some(&cancel), None) {
        Ok(wl) => println!(
            "dirs={} small={} large={} total_files={}",
            wl.dirs.len(),
            wl.small_files.len(),
            wl.large_files.len(),
            wl.total_files
        ),
        Err(e) => println!("ERR: {e}"),
    }

    println!("=== run_engine full ===");
    let s: EngineSummary = run_engine(
        &[src.clone()],
        &dest,
        false,
        None,
        cancel.clone(),
        pause,
        true,
        None,
    );
    println!("files_copied={} bytes={} failures={} cancelled={}", s.files_copied, s.bytes_copied, s.failures.len(), s.was_cancelled);
    for (p, m) in &s.failures {
        println!("  FAIL {} => {}", p.display(), m);
    }
    println!("=== dest tree ===");
    for p in list_tree(&dest) {
        println!("  {}", p);
    }
    println!("=== src still exists? {} ===", src.exists());
    println!("=== summary check: files_copied={} failures={} ===", s.files_copied, s.failures.len());
    assert_eq!(s.files_copied, 15);
    assert!(s.failures.is_empty(), "failures: {:?}", s.failures);

    let _ = fs::remove_dir_all(&root);
}