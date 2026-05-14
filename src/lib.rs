mod app;
mod device;
mod gcode;
mod model;
mod svg_toolpath;
mod viewer;

use app::PenarticApp;

pub use app::PenarticApp as App;

#[cfg(not(target_arch = "wasm32"))]
pub fn run_native() -> eframe::Result {
    let native_options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        multisampling: 4,
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1440.0, 920.0])
            .with_min_inner_size([1100.0, 720.0])
            .with_title("Penartic"),
        ..Default::default()
    };

    eframe::run_native(
        "Penartic",
        native_options,
        Box::new(|cc| Ok(Box::new(PenarticApp::new(cc)))),
    )
}

#[cfg(target_arch = "wasm32")]
pub fn start_web() -> Result<(), wasm_bindgen::JsValue> {
    use wasm_bindgen::JsCast as _;

    console_error_panic_hook::set_once();
    eframe::WebLogger::init(log::LevelFilter::Info).ok();

    wasm_bindgen_futures::spawn_local(async {
        let document =
            eframe::web_sys::window().and_then(|window| window.document()).expect("web document");
        let canvas = document
            .get_element_by_id("penartic-canvas")
            .expect("canvas element with id 'penartic-canvas'")
            .dyn_into::<eframe::web_sys::HtmlCanvasElement>()
            .expect("penartic-canvas should be a canvas element");

        let web_options = eframe::WebOptions::default();

        eframe::WebRunner::new()
            .start(canvas, web_options, Box::new(|cc| Ok(Box::new(PenarticApp::new(cc)))))
            .await
            .expect("failed to start Penartic web app");
    });

    Ok(())
}
