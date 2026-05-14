#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    penartic_app::run_native()
}

#[cfg(target_arch = "wasm32")]
fn main() {
    penartic_app::start_web().expect("failed to start Penartic web app");
}
