use std::thread;
use std::time::Duration;
use better_copy_core::trigger::HotkeyTrigger;

fn main() {
    println!("Starting Hotkey Interceptor test...");
    println!("Please open Windows Explorer or switch focus to the Desktop, copy some files (Ctrl+C), and then press Ctrl+Shift+V.");
    
    let trigger = match HotkeyTrigger::start(|clipboard, dest| {
        println!("\n*** HOTKEY TRIGGERED ***");
        println!("Is Move (Cut): {}", clipboard.is_move);
        println!("Sources (copied paths):");
        for path in clipboard.paths {
            println!("  - {:?}", path);
        }
        println!("Resolved Destination: {:?}", dest);
    }) {
        Ok(t) => t,
        Err(e) => {
            println!("Failed to start trigger: {}", e);
            return;
        }
    };
    
    println!("Monitoring... Press Ctrl+C in this terminal to exit.");
    for i in 1..=60 {
        thread::sleep(Duration::from_secs(1));
        if i % 10 == 0 {
            println!("Still monitoring... ({}s elapsed)", i);
        }
    }
    
    println!("Test finished. Dropping trigger.");
    drop(trigger);
}
