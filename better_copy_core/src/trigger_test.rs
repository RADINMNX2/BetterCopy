use std::thread;
use std::time::Duration;
use better_copy_core::trigger::HotkeyTrigger;

fn main() {
    println!("Starting Hotkey Interceptor test...");
    println!("Please open Windows Explorer or switch focus to the Desktop, and try pressing both:");
    println!("  1. Ctrl+Shift+V");
    println!("  2. Ctrl+Shift+Delete");
    
    let trigger = match HotkeyTrigger::start(|event| {
        match event {
            better_copy_core::trigger::HotkeyEvent::Paste { clipboard, destination } => {
                println!("\n*** PASTE EVENT TRIGGERED ***");
                println!("Sources (copied paths):");
                for path in clipboard.paths {
                    println!("  - {:?}", path);
                }
                println!("Resolved Destination: {:?}", destination);
            }
            better_copy_core::trigger::HotkeyEvent::Delete { sources } => {
                println!("\n*** DELETE EVENT TRIGGERED ***");
                println!("Sources to delete:");
                for path in sources {
                    println!("  - {:?}", path);
                }
            }
        }
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
