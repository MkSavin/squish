use std::sync::atomic::{AtomicBool, Ordering};

static REQUESTED: AtomicBool = AtomicBool::new(false);

/// Ctrl+C asks the workers to stop and kill their ffmpeg; pressing it again
/// gives up on a clean stop. Without this the console signal can reach ffmpeg
/// but not squish, leaving an encode running with nothing watching it.
pub fn install() {
    let installed = ctrlc::set_handler(|| {
        if REQUESTED.swap(true, Ordering::SeqCst) {
            eprintln!("squish: giving up on a clean stop");
            std::process::exit(130);
        }
        eprintln!(
            "\nsquish: stopping after the running encodes are killed (Ctrl+C again to force)"
        );
    });
    if installed.is_err() {
        eprintln!("squish: could not listen for Ctrl+C; stop the run with the terminal instead");
    }
}

pub fn requested() -> bool {
    REQUESTED.load(Ordering::Relaxed)
}

/// Waits, but stays responsive to Ctrl+C. False when the wait was cut short.
pub fn sleep(total: std::time::Duration) -> bool {
    const STEP: std::time::Duration = std::time::Duration::from_millis(100);
    let deadline = std::time::Instant::now() + total;
    while std::time::Instant::now() < deadline {
        if requested() {
            return false;
        }
        std::thread::sleep(STEP.min(deadline - std::time::Instant::now()));
    }
    !requested()
}
