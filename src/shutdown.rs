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
