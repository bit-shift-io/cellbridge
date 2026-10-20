mod api;
mod config;
mod device;
mod frb_generated; /* AUTO INJECTED BY flutter_rust_bridge. This line may not be accurate, and you can change it according to your needs. */
mod gateway;
mod tunnel;
pub use api::*;
pub use config::*;
pub use device::*;
pub use gateway::*;
pub use tunnel::*;

use flutter_rust_bridge::frb;

#[frb(init)]
fn init() {
    flutter_rust_bridge::setup_default_user_utils();
}
