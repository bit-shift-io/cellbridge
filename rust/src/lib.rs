#[cfg(target_os = "android")]
mod android_jni;
mod api;
mod bridge;
mod config;
mod device;
pub mod diagnostics;
mod frb_generated; /* AUTO INJECTED BY flutter_rust_bridge. This line may not be accurate, and you can change it according to your needs. */
mod gateway;
mod keys;
pub mod native;
mod stats;
mod tcp;
pub mod test_peer;
mod tunnel;
mod udp;
pub use api::*;
pub use config::*;
pub use device::*;
pub use gateway::*;
pub use stats::*;
pub use tcp::*;
pub use tunnel::*;
pub use udp::*;

use flutter_rust_bridge::frb;

#[frb(init)]
fn init() {
    flutter_rust_bridge::setup_default_user_utils();
}
pub mod connection_test;
