use std::env;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use better_copy_core::engine::run_engine;

fn generate_fixture(dest: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dest)?;
    
    // 8 KB buffer of zeroes
    let buffer = vec![0u8; 8192];
    
    for d in 0..500 {
        let dir_path = dest.join(format!("dir_{}", d));
        fs::create_dir_all(&dir_path)?;
        
        for f in 0..200 {
            let file_path = dir_path.join(format!("file_{}.bin", f));
            let mut file = File::create(&file_path)?;
            file.write_all(&buffer)?;
        }
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = env::args().collect();
    
    // Check if user is generating the benchmark fixture
    if args.len() == 3 && args[1] == "--gen-fixture" {
        let dest_path = PathBuf::from(&args[2]);
        println!("Generating 100,000 tiny-file fixture at {}...", dest_path.display());
        let start = std::time::Instant::now();
        if let Err(e) = generate_fixture(&dest_path) {
            eprintln!("Error generating fixture: {}", e);
            std::process::exit(1);
        }
        println!("Fixture generated successfully in {:?}", start.elapsed());
        std::process::exit(0);
    }

    if args.len() < 3 {
        eprintln!("Usage: bcopy [--move] <sources...> <destination>");
        eprintln!("       bcopy --gen-fixture <directory>");
        std::process::exit(1);
    }
    
    let mut is_move = false;
    let mut custom_concurrency = None;
    let mut positionals = Vec::new();
    
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--move" || args[i] == "-m" {
            is_move = true;
        } else if (args[i] == "--threads" || args[i] == "-t") && i + 1 < args.len() {
            if let Ok(n) = args[i + 1].parse::<usize>() {
                custom_concurrency = Some(n);
            }
            i += 1;
        } else {
            positionals.push(args[i].clone());
        }
        i += 1;
    }

    // Parse verbs: check if first positional is "copy" or "move"
    if !positionals.is_empty() {
        if positionals[0] == "copy" {
            positionals.remove(0);
        } else if positionals[0] == "move" {
            is_move = true;
            positionals.remove(0);
        }
    }
    
    if positionals.len() < 2 {
        eprintln!("Error: Missing source paths or destination path.");
        eprintln!("Usage: bcopy [copy|move] [--threads N] <sources...> <destination>");
        std::process::exit(1);
    }
    
    let dest = positionals.pop().unwrap();
    let sources: Vec<PathBuf> = positionals.into_iter().map(PathBuf::from).collect();
    let dest_path = PathBuf::from(dest);
    
    let profile = better_copy_core::profiler::profile_device(&dest_path);
    let concurrency_used = custom_concurrency.unwrap_or(profile.concurrency);
    
    println!("Starting BetterCopy Engine...");
    println!("Sources: {:?}", sources);
    println!("Destination: {} (Profile: {}, Concurrency: {} threads)", dest_path.display(), profile.description, concurrency_used);
    println!("Operation: {}", if is_move { "Move (Cut)" } else { "Copy" });
    
    let cancel_flag = Arc::new(AtomicBool::new(false));
    
    // Setup graceful Ctrl+C cancellation
    let c_flag = cancel_flag.clone();
    if let Err(e) = ctrlc::set_handler(move || {
        println!("\n[Ctrl+C] Cancellation requested. Cleaning up partial transfers...");
        c_flag.store(true, std::sync::atomic::Ordering::SeqCst);
    }) {
        eprintln!("Warning: Failed to set Ctrl+C handler: {}", e);
    }
    
    let summary = run_engine(
        &sources,
        &dest_path,
        is_move,
        custom_concurrency,
        cancel_flag,
        None,
    );
    
    println!("\n");
    println!("--- Transfer Summary ---");
    println!("Status: {}", if summary.was_cancelled { "CANCELLED" } else if !summary.failures.is_empty() { "FAILED" } else { "SUCCESS" });
    println!("Files Copied: {}", summary.files_copied);
    println!("Bytes Copied: {} ({:.2} MB)", summary.bytes_copied, summary.bytes_copied as f64 / 1_048_576.0);
    println!("Elapsed Time: {:?}", summary.elapsed);
    if !summary.failures.is_empty() {
        eprintln!("\nFailures:");
        for (path, err) in &summary.failures {
            eprintln!("  - {}: {}", path.display(), err);
        }
        std::process::exit(1);
    } else if summary.was_cancelled {
        std::process::exit(130);
    } else {
        std::process::exit(0);
    }
}
