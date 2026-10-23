use std::io::BufRead;
use tokio::sync::mpsc;

/// A dedicated OS thread avoids tokio's uncancellable stdin blocking task,
/// which otherwise prevents runtime shutdown after Ctrl-C.
pub fn input() -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel(8);
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if tx.blocking_send(line).is_err() {
                break;
            }
        }
    });
    rx
}
