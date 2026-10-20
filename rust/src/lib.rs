mod api;
mod frb_generated; /* AUTO INJECTED BY flutter_rust_bridge. This line may not be accurate, and you can change it according to your needs. */
pub use api::*;

use flutter_rust_bridge::frb;

#[frb(init)]
fn init() {
    flutter_rust_bridge::setup_default_user_utils();
}
