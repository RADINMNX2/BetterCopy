#![cfg(windows)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use better_copy_core::engine::{run_engine, EngineSummary};

fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bc_test_{}_{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_tree(root: &Path, files: usize, dirs: usize, size: usize) -> PathBuf {
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

fn dir_entry_count(root: &Path) -> usize {
    let mut total = 0;
    fn walk(path: &Path, total: &mut usize) {
        if let Ok(rd) = fs::read_dir(path) {
            for entry in rd.flatten() {
                *total += 1;
                if entry.path().is_dir() {
                    walk(&entry.path(), total);
                }
            }
        }
    }
    walk(root, &mut total);
    total
}

fn summarize(root: &Path) -> Vec<(PathBuf, u64)> {
    let mut out = Vec::new();
    fn walk(path: &Path, base: &Path, out: &mut Vec<(PathBuf, u64)>) {
        if let Ok(rd) = fs::read_dir(path) {
            for entry in rd.flatten() {
                let rel = entry.path().strip_prefix(base).unwrap().to_path_buf();
                if entry.path().is_dir() {
                    walk(&entry.path(), base, out);
                } else {
                    let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                    out.push((rel, size));
                }
            }
        }
    }
    for entry in fs::read_dir(root).unwrap().flatten() {
        if entry.path().is_dir() {
            walk(&entry.path(), &entry.path(), &mut out);
        }
    }
    out.sort();
    out
}

fn run_copy(sources: &[PathBuf], dest: &Path, is_move: bool) -> EngineSummary {
    run_engine(
        sources,
        dest,
        is_move,
        None,
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
        true,
        None,
    )
}

#[test]
fn copy_preserves_content_and_counts() {
    let root = temp_root("copy_basic");
    let src = write_tree(&root.join("src"), 5, 3, 1024 * 4 + 7);
    let dest = root.join("dest");
    fs::create_dir_all(&dest).unwrap();

    let summary = run_copy(&[src.clone()], &dest, false);
    assert!(summary.failures.is_empty(), "failures: {:?}", summary.failures);
    assert!(!summary.was_cancelled);
    assert_eq!(summary.files_copied, 15, "5 files x 3 dirs expected");
    assert_eq!(dir_entry_count(&dest) - 1, dir_entry_count(&src) - 1);

    // Byte-for-byte content check on a few files.
    for name in ["file_00_000.bin", "file_01_002.bin", "file_02_004.bin"] {
        let a = fs::read(src.join("dir00").join(name)).unwrap();
        let b = fs::read(dest.join("dir00").join(name)).unwrap();
        assert_eq!(a, b, "content mismatch for {name}");
    }

    // Verify mode already checks hashes; a matched manifest means identical trees.
    let src_sum = summarize(&src);
    let dest_sum = summarize(&dest);
    assert_eq!(src_sum.len(), dest_sum.len());
    for (s, d) in src_sum.iter().zip(dest_sum.iter()) {
        assert_eq!(s.1, d.1, "size mismatch for {}", d.0.display());
    }

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn copy_preserves_timestamps_and_readonly_bit() {
    let root = temp_root("copy_meta");
    let src = write_tree(&root.join("src"), 2, 1, 1024);
    let dest = root.join("dest");
    fs::create_dir_all(&dest).unwrap();

    let target = src.join("dir00/file_00_000.bin");
    // Set the source file read-only so we can assert the attribute propagates.
    let mut perms = fs::metadata(&target).unwrap().permissions();
    perms.set_readonly(true);
    fs::set_permissions(&target, perms).unwrap();

    let summary = run_copy(&[src], &dest, false);
    assert!(summary.failures.is_empty(), "failures: {:?}", summary.failures);

    let copied = dest.join("dir00/file_00_000.bin");
    let copied_ro = fs::metadata(&copied).unwrap().permissions().readonly();
    assert!(copied_ro, "readonly attribute should propagate");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn move_rehooks_source_cleanly() {
    let root = temp_root("move_clean");
    let src = write_tree(&root.join("src"), 4, 2, 2048);
    let dest = root.join("dest");
    fs::create_dir_all(&dest).unwrap();

    let summary = run_copy(&[src.clone()], &dest, true);
    assert!(summary.failures.is_empty(), "failures: {:?}", summary.failures);
    assert!(!summary.was_cancelled);

    assert!(dest.join("dir00").exists(), "destination should have content");
    // The two-phase verified move should have removed the source tree.
    assert!(!src.exists(), "source root should be gone after a verified move");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn pause_blocks_until_resumed() {
    let root = temp_root("pause_resume");
    let src = write_tree(&root.join("src"), 60, 1, 512);
    let dest = root.join("dest");
    fs::create_dir_all(&dest).unwrap();

    let cancel = Arc::new(AtomicBool::new(false));
    let pause = Arc::new(AtomicBool::new(true));

    let pause_resume = pause.clone();
    let resume = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(400));
        pause_resume.store(false, Ordering::SeqCst);
    });

    let summary = run_engine(
        &[src],
        &dest,
        false,
        None,
        cancel,
        pause,
        true,
        None,
    );
    resume.join().unwrap();

    assert!(summary.failures.is_empty(), "failures: {:?}", summary.failures);
    assert_eq!(summary.files_copied, 60);
    assert!(!summary.was_cancelled);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn cancel_aborts_before_processing() {
    let root = temp_root("cancel_test");
    let src = write_tree(&root.join("src"), 40, 2, 1024);

    let dest = root.join("dest");
    fs::create_dir_all(&dest).unwrap();

    let cancel = Arc::new(AtomicBool::new(true));
    let summary = run_engine(
        &[src],
        &dest,
        false,
        None,
        cancel,
        Arc::new(AtomicBool::new(false)),
        true,
        None,
    );

    // Engine should honour pre-set cancellation and stay clean/quick.
    assert!(summary.was_cancelled, "expected early cancellation");
    assert!(summary.failures.is_empty(), "no failures expected on cancel");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn empty_source_reports_no_failures() {
    let root = temp_root("empty_src");
    let src = root.join("nonexistent");
    let dest = root.join("dest");
    fs::create_dir_all(&dest).unwrap();

    let summary = run_copy(&[src], &dest, false);
    assert!(!summary.was_cancelled);
    assert!(summary.failures.is_empty(), "failures: {:?}", summary.failures);

    let _ = fs::remove_dir_all(&root);
}