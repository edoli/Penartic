mod english;
mod korean;

use std::fmt::Display;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    #[default]
    English,
    Korean,
}

impl Language {
    pub const ALL: [Self; 2] = [Self::English, Self::Korean];

    pub fn strings(self) -> &'static Strings {
        match self {
            Self::English => &english::STRINGS,
            Self::Korean => &korean::STRINGS,
        }
    }

    pub fn native_name(self) -> &'static str {
        self.strings().native_language_name
    }

    pub fn storage_key(self) -> &'static str {
        match self {
            Self::English => "english",
            Self::Korean => "korean",
        }
    }

    pub fn from_storage_key(value: &str) -> Option<Self> {
        match value {
            "english" => Some(Self::English),
            "korean" => Some(Self::Korean),
            _ => None,
        }
    }
}

pub struct Strings {
    pub native_language_name: &'static str,
    pub failed_to_read_startup_svg_file: &'static str,
    pub failed_to_parse_svg: &'static str,
    pub no_drawable_paths_in_svg: &'static str,
    pub text_nodes_not_converted: &'static str,
    pub image_nodes_not_converted: &'static str,
    pub language_label: &'static str,
    pub load_svg: &'static str,
    pub load_svg_hint: &'static str,
    pub copy_gcode: &'static str,
    pub failed_to_read_svg_file: &'static str,
    pub only_svg_drag_drop: &'static str,
    pub failed_to_read_dropped_svg_file: &'static str,
    pub dropped_svg_not_ready: &'static str,
    pub device_heading: &'static str,
    pub status_label: &'static str,
    pub job_label: &'static str,
    pub firmware_label: &'static str,
    pub detected_size_prefix: &'static str,
    pub refresh_ports: &'static str,
    pub use_esp3d_connection: &'static str,
    pub esp3d_address: &'static str,
    pub esp3d_device: &'static str,
    pub esp3d_http_ready: &'static str,
    pub esp3d_http_connected: &'static str,
    pub esp3d_http_request_failed: &'static str,
    pub secure_websocket_required: &'static str,
    pub select_port: &'static str,
    pub connect: &'static str,
    pub disconnect: &'static str,
    pub start_print: &'static str,
    pub stop_print: &'static str,
    pub home_xy_before_print: &'static str,
    pub direct_start_without_home_hint: &'static str,
    pub move_to_first_start_point: &'static str,
    pub move_to_current_position: &'static str,
    pub bounding_box_corners: &'static str,
    pub top_left: &'static str,
    pub top_right: &'static str,
    pub bottom_left: &'static str,
    pub bottom_right: &'static str,
    pub motors_off: &'static str,
    pub settings_heading: &'static str,
    pub printable_width: &'static str,
    pub printable_height: &'static str,
    pub print_speed: &'static str,
    pub z_lift: &'static str,
    pub use_arc_gcode: &'static str,
    pub use_bezier_gcode: &'static str,
    pub arc_and_bezier_export_hint: &'static str,
    pub arc_export_hint: &'static str,
    pub bezier_export_hint: &'static str,
    pub round_sharp_corners: &'static str,
    pub corner_rounding_radius: &'static str,
    pub corner_rounding_start_angle: &'static str,
    pub corner_rounding_hint: &'static str,
    pub fill_closed_shapes: &'static str,
    pub fill_pattern_lines: &'static str,
    pub fill_pattern_crosshatch: &'static str,
    pub fill_pattern_zigzag: &'static str,
    pub fill_density: &'static str,
    pub fill_angle: &'static str,
    pub fill_density_hint: &'static str,
    pub svg_placement_heading: &'static str,
    pub svg_center_x: &'static str,
    pub svg_center_y: &'static str,
    pub svg_scale: &'static str,
    pub current_size_prefix: &'static str,
    pub svg_scale_hint: &'static str,
    pub svg_out_of_bounds: &'static str,
    pub load_svg_to_adjust_position: &'static str,
    pub job_info_heading: &'static str,
    pub drawing_bounds_prefix: &'static str,
    pub stroke_count_prefix: &'static str,
    pub segment_count_prefix: &'static str,
    pub drawing_distance_prefix: &'static str,
    pub travel_distance_prefix: &'static str,
    pub estimated_duration_prefix: &'static str,
    pub no_converted_svg: &'static str,
    pub device_log_heading: &'static str,
    pub jog_step: &'static str,
    pub manual_control_unavailable: &'static str,
    pub play: &'static str,
    pub pause: &'static str,
    pub reset: &'static str,
    pub show_pen_lift_travel_paths: &'static str,
    pub show_bounding_box: &'static str,
    pub object_position_label: &'static str,
    pub object_scale_label: &'static str,
    pub object_rotation_label: &'static str,
    pub object_lock_aspect_ratio: &'static str,
    pub object_move_tool: &'static str,
    pub object_scale_tool: &'static str,
    pub object_rotate_tool: &'static str,
    pub object_delete_short: &'static str,
    pub delete_selected_svg: &'static str,
    pub no_svg_selected: &'static str,
    pub load_svg_to_use_control: &'static str,
    pub wgpu_preview_unavailable: &'static str,
    pub load_svg_preview_placeholder: &'static str,
    pub g2g3_firmware_warning: &'static str,
    pub g5_firmware_warning: &'static str,
    pub web_serial_available: &'static str,
    pub web_serial_unsupported: &'static str,
    pub web_serial_choose_port_hint: &'static str,
    pub web_preview_only: &'static str,
    pub disconnected: &'static str,
    pub connecting: &'static str,
    pub connected: &'static str,
    pub failed_to_read_port_list: &'static str,
    pub idle: &'static str,
    pub printing: &'static str,
    pub stopping: &'static str,
    pub opening_browser_port_picker: &'static str,
    pub select_serial_port_before_connecting: &'static str,
    pub failed_to_start_initial_probe: &'static str,
    pub closed_device_connection: &'static str,
    pub print_already_in_progress: &'static str,
    pub connect_device_first: &'static str,
    pub failed_to_queue_gcode_to_device: &'static str,
    pub no_active_print_job: &'static str,
    pub requested_print_stop: &'static str,
    pub failed_to_send_stop_command: &'static str,
    pub no_axis_to_move: &'static str,
    pub sent_manual_xy_move: &'static str,
    pub sent_manual_z_move: &'static str,
    pub sent_xy_home: &'static str,
    pub sent_z_home: &'static str,
    pub sent_motors_off: &'static str,
    pub sent_move_to_first_start: &'static str,
    pub sent_absolute_move: &'static str,
    pub opened_serial_port_waiting_firmware: &'static str,
    pub device_error_prefix: &'static str,
    pub printing_completed: &'static str,
    pub printing_stopped: &'static str,
    pub manual_control_unavailable_while_printing: &'static str,
    pub failed_to_send_manual_control_command: &'static str,
    pub select_port_in_browser: &'static str,
    pub web_serial_device: &'static str,
    pub firmware_ready_timeout: &'static str,
    pub found_serial_ports_suffix: &'static str,
    pub trying_to_connect_prefix: &'static str,
    pub trying_to_connect_suffix: &'static str,
    pub queued_gcode_lines_prefix: &'static str,
    pub queued_gcode_lines_suffix: &'static str,
    pub busy_waiting_command_prefix: &'static str,
    pub busy_waiting_command_suffix: &'static str,
}

impl Strings {
    pub fn startup_svg_read_failed(&self, path: impl Display, error: impl Display) -> String {
        format!("{} ({}): {error}", self.failed_to_read_startup_svg_file, path)
    }

    pub fn parse_svg_failed(&self, error: impl Display) -> String {
        format!("{}: {error}", self.failed_to_parse_svg)
    }

    pub fn read_svg_file_failed(&self, error: impl Display) -> String {
        format!("{}: {error}", self.failed_to_read_svg_file)
    }

    pub fn read_dropped_svg_file_failed(&self, error: impl Display) -> String {
        format!("{}: {error}", self.failed_to_read_dropped_svg_file)
    }

    pub fn detected_size(&self, width_mm: f32, height_mm: f32) -> String {
        format!("{} {:.0} x {:.0} mm", self.detected_size_prefix, width_mm, height_mm)
    }

    pub fn current_size(&self, width_mm: f32, height_mm: f32) -> String {
        format!("{} {:.1} x {:.1} mm", self.current_size_prefix, width_mm, height_mm)
    }

    pub fn drawing_bounds(&self, width_mm: f32, height_mm: f32) -> String {
        format!("{} {:.1} x {:.1} mm", self.drawing_bounds_prefix, width_mm, height_mm)
    }

    pub fn stroke_count(&self, count: usize) -> String {
        format!("{} {count}", self.stroke_count_prefix)
    }

    pub fn segment_count(&self, count: usize) -> String {
        format!("{} {count}", self.segment_count_prefix)
    }

    pub fn drawing_distance(&self, distance_mm: f32) -> String {
        format!("{} {:.1} mm", self.drawing_distance_prefix, distance_mm)
    }

    pub fn travel_distance(&self, distance_mm: f32) -> String {
        format!("{} {:.1} mm", self.travel_distance_prefix, distance_mm)
    }

    pub fn estimated_duration(&self, duration_s: f32) -> String {
        format!("{} {:.1} s", self.estimated_duration_prefix, duration_s)
    }

    pub fn found_serial_ports(&self, count: usize) -> String {
        format!("{count}{}", self.found_serial_ports_suffix)
    }

    pub fn failed_to_read_port_list(&self, error: impl Display) -> String {
        format!("{}: {error}", self.failed_to_read_port_list)
    }

    pub fn connected_status(&self, port: &str) -> String {
        format!("{}: {port}", self.connected)
    }

    pub fn trying_to_connect(&self, port: &str) -> String {
        format!("{}{}{}", self.trying_to_connect_prefix, port, self.trying_to_connect_suffix)
    }

    pub fn queued_gcode_lines(&self, count: usize) -> String {
        format!("{}{}{}", self.queued_gcode_lines_prefix, count, self.queued_gcode_lines_suffix)
    }

    pub fn device_error(&self, message: &str) -> String {
        format!("{}: {message}", self.device_error_prefix)
    }

    pub fn busy_waiting_command(&self, line: &str, waiting_line: &str) -> String {
        format!(
            "{line}{}{}{}",
            self.busy_waiting_command_prefix, waiting_line, self.busy_waiting_command_suffix
        )
    }
}

#[cfg(test)]
mod tests {
    use super::Language;

    #[test]
    fn default_language_is_english() {
        assert_eq!(Language::default(), Language::English);
    }

    #[test]
    fn storage_keys_round_trip() {
        assert_eq!(Language::from_storage_key("english"), Some(Language::English));
        assert_eq!(Language::from_storage_key("korean"), Some(Language::Korean));
        assert_eq!(Language::English.storage_key(), "english");
        assert_eq!(Language::Korean.storage_key(), "korean");
    }
}
