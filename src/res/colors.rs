use eframe::egui::Color32;

pub fn success() -> Color32 {
    Color32::from_rgb(87, 201, 112)
}

pub fn warning() -> Color32 {
    Color32::from_rgb(232, 191, 79)
}

pub fn error() -> Color32 {
    Color32::from_rgb(230, 92, 92)
}

pub fn muted_text() -> Color32 {
    Color32::from_rgb(177, 184, 196)
}

pub fn preview_background() -> Color32 {
    Color32::from_rgb(14, 18, 24)
}

pub fn preview_overlay_background() -> Color32 {
    Color32::from_rgba_unmultiplied(14, 18, 24, 212)
}

pub fn preview_plane() -> [f32; 4] {
    [0.08, 0.11, 0.16, 1.0]
}

pub fn preview_edge() -> [f32; 4] {
    [0.46, 0.54, 0.66, 1.0]
}

pub fn preview_grid() -> [f32; 4] {
    [0.22, 0.28, 0.36, 1.0]
}

pub fn preview_travel() -> [f32; 4] {
    [0.72, 0.72, 0.74, 1.0]
}

pub fn preview_draw() -> [f32; 4] {
    [0.24, 0.72, 1.0, 1.0]
}

pub fn preview_overflow() -> [f32; 4] {
    [0.96, 0.72, 0.25, 1.0]
}

pub fn preview_pen_base() -> [f32; 4] {
    [0.96, 0.42, 0.28, 1.0]
}

pub fn preview_pen_cap() -> [f32; 4] {
    [0.84, 0.24, 0.18, 1.0]
}
