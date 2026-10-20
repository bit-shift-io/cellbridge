mod api;
mod bridge;
mod config;
mod device;
mod frb_generated; /* AUTO INJECTED BY flutter_rust_bridge. This line may not be accurate, and you can change it according to your needs. */
mod gateway;
mod keys;
mod stats;
mod tcp;
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
