use std::{collections::VecDeque, time::Duration};

use crate::{
    plot::model::PrintableArea,
    res::lang::{Language, Strings},
};

#[cfg(target_arch = "wasm32")]
use {
    js_sys::{ArrayBuffer, Function, Object, Reflect, Uint8Array},
    std::{cell::RefCell, rc::Rc},
    wasm_bindgen::{JsCast as _, JsValue, closure::Closure},
    wasm_bindgen_futures::{JsFuture, spawn_local},
    web_sys::{BinaryType, CloseEvent, Event, MessageEvent, WebSocket},
};

const DEVICE_LOG_LIMIT: usize = 48;
const DEFAULT_ESP3D_ENDPOINT: &str = "http://192.168.0.112/";
const ESP3D_DEFAULT_DATA_WEBSOCKET_PORT: u16 = 8282;
const ESP3D_MAX_IN_FLIGHT_LINES: usize = 32;
const ESP3D_TOP_UP_LINES_PER_TICK: usize = 16;
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
const MAX_IN_FLIGHT_LINES: usize = 1;
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
const READY_PING_INTERVAL: Duration = Duration::from_millis(500);
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
const READY_TIMEOUT: Duration = Duration::from_secs(15);

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Unsupported,
    Disconnected,
    Connecting,
    Connected,
}

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrintState {
    Idle,
    Printing,
    Stopping,
}

pub struct DeviceController {
    language: Language,
    available_ports: Vec<String>,
    selected_port: Option<String>,
    connection_state: ConnectionState,
    print_state: PrintState,
    firmware_summary: Option<String>,
    detected_area: Option<PrintableArea>,
    log: VecDeque<String>,
    last_error: Option<String>,
    use_esp3d: bool,
    esp3d_endpoint: String,
    #[cfg(not(target_arch = "wasm32"))]
    worker: Option<NativeWorker>,
    #[cfg(target_arch = "wasm32")]
    worker: Option<WebWorker>,
}

impl DeviceController {
    fn text(&self) -> &'static Strings {
        self.language.strings()
    }

    pub fn new(language: Language) -> Self {
        #[allow(unused_mut)]
        let mut controller = Self {
            language,
            available_ports: Vec::new(),
            selected_port: None,
            connection_state: ConnectionState::Disconnected,
            print_state: PrintState::Idle,
            firmware_summary: None,
            detected_area: None,
            log: VecDeque::new(),
            last_error: None,
            use_esp3d: false,
            esp3d_endpoint: DEFAULT_ESP3D_ENDPOINT.to_owned(),
            #[cfg(not(target_arch = "wasm32"))]
            worker: None,
            #[cfg(target_arch = "wasm32")]
            worker: None,
        };

        #[cfg(target_arch = "wasm32")]
        {
            if web_serial_api().is_some() {
                controller.selected_port =
                    Some(browser_port_selection_label(controller.language).to_owned());
                controller
                    .available_ports
                    .push(browser_port_selection_label(controller.language).to_owned());
                controller.push_log(controller.text().web_serial_available);
            } else {
                controller.push_log(web_serial_unsupported_message(controller.language));
            }
        }

        controller
    }

    pub fn set_language(&mut self, language: Language) {
        #[cfg(target_arch = "wasm32")]
        let previous = self.language;
        self.language = language;

        #[cfg(target_arch = "wasm32")]
        self.update_web_port_labels(previous);
    }

    #[cfg(target_arch = "wasm32")]
    fn update_web_port_labels(&mut self, previous: Language) {
        relabel_port_entry(
            &mut self.selected_port,
            previous,
            self.language,
            browser_port_selection_label,
        );
        relabel_port_entry(
            &mut self.selected_port,
            previous,
            self.language,
            web_serial_device_label,
        );
        relabel_esp3d_target_entry(&mut self.selected_port, previous, self.language);
        for port in &mut self.available_ports {
            relabel_port_value(port, previous, self.language, browser_port_selection_label);
            relabel_port_value(port, previous, self.language, web_serial_device_label);
        }
    }

    pub fn refresh_ports(&mut self) {
        #[cfg(target_arch = "wasm32")]
        {
            self.available_ports.clear();
            if web_serial_api().is_some() {
                if !self.use_esp3d {
                    self.selected_port =
                        Some(browser_port_selection_label(self.language).to_owned());
                }
                self.available_ports.push(browser_port_selection_label(self.language).to_owned());
                self.push_log(self.text().web_serial_choose_port_hint);
            } else {
                if !self.use_esp3d {
                    self.selected_port = None;
                }
                self.push_log(web_serial_unsupported_message(self.language));
            }

            if self.use_esp3d {
                self.push_log(self.text().esp3d_http_ready);
            }
            return;
        }

        #[cfg(not(target_arch = "wasm32"))]
        match serialport::available_ports() {
            Ok(ports) => {
                self.available_ports = ports.into_iter().map(|port| port.port_name).collect();
                if self.selected_port.is_none() {
                    self.selected_port = self.available_ports.first().cloned();
                }
                self.push_log(self.text().found_serial_ports(self.available_ports.len()));
                self.last_error = None;
            }
            Err(error) => {
                self.available_ports.clear();
                self.last_error = Some(error.to_string());
                self.push_log(self.text().failed_to_read_port_list(error));
            }
        }

        #[cfg(not(target_arch = "wasm32"))]
        if self.use_esp3d {
            self.push_log(self.text().esp3d_http_ready);
        }
    }

    pub fn ports(&self) -> &[String] {
        &self.available_ports
    }

    pub fn selected_port(&self) -> Option<&str> {
        self.selected_port.as_deref()
    }

    pub fn set_selected_port(&mut self, selected_port: Option<String>) {
        self.selected_port = selected_port;
    }

    pub fn use_esp3d(&self) -> bool {
        self.use_esp3d
    }

    pub fn set_use_esp3d(&mut self, use_esp3d: bool) {
        self.use_esp3d = use_esp3d;
        if use_esp3d {
            self.push_log(self.text().esp3d_http_ready);
        } else if self
            .selected_port
            .as_ref()
            .is_none_or(|selected| !self.available_ports.iter().any(|port| port == selected))
        {
            self.selected_port = self.available_ports.first().cloned();
        }
    }

    pub fn esp3d_endpoint(&self) -> &str {
        &self.esp3d_endpoint
    }

    pub fn set_esp3d_endpoint(&mut self, endpoint: String) {
        self.esp3d_endpoint = endpoint;
    }

    pub fn connection_state(&self) -> ConnectionState {
        self.connection_state
    }

    pub fn status_text(&self) -> String {
        let text = self.text();
        match self.connection_state {
            ConnectionState::Unsupported => text.web_preview_only.to_owned(),
            ConnectionState::Disconnected => text.disconnected.to_owned(),
            ConnectionState::Connecting => text.connecting.to_owned(),
            ConnectionState::Connected => match &self.selected_port {
                Some(port) => text.connected_status(port),
                None => text.connected.to_owned(),
            },
        }
    }

    pub fn print_state(&self) -> PrintState {
        self.print_state
    }

    pub fn print_state_text(&self) -> &'static str {
        let text = self.text();
        match self.print_state {
            PrintState::Idle => text.idle,
            PrintState::Printing => text.printing,
            PrintState::Stopping => text.stopping,
        }
    }

    pub fn firmware_summary(&self) -> Option<&str> {
        self.firmware_summary.as_deref()
    }

    pub fn detected_area(&self) -> Option<PrintableArea> {
        self.detected_area
    }

    pub fn is_connected(&self) -> bool {
        self.connection_state == ConnectionState::Connected
    }

    pub fn is_job_active(&self) -> bool {
        matches!(self.print_state, PrintState::Printing | PrintState::Stopping)
    }

    pub fn can_stop_print(&self) -> bool {
        self.is_connected() && self.is_job_active()
    }

    pub fn can_connect(&self) -> bool {
        #[cfg(target_arch = "wasm32")]
        {
            if self.use_esp3d {
                !self.esp3d_endpoint.trim().is_empty()
            } else {
                web_serial_api().is_some()
            }
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.use_esp3d {
                !self.esp3d_endpoint.trim().is_empty()
            } else {
                !self.available_ports.is_empty()
            }
        }
    }

    pub fn needs_poll(&self) -> bool {
        #[cfg(target_arch = "wasm32")]
        {
            self.worker.is_some()
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            self.worker.is_some()
        }
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn log_lines(&self) -> impl DoubleEndedIterator<Item = &str> {
        self.log.iter().map(String::as_str)
    }

    pub fn connect(&mut self) -> Result<(), String> {
        #[cfg(target_arch = "wasm32")]
        {
            let target = if self.use_esp3d {
                WebConnectionTarget::Esp3d { endpoint: self.esp3d_endpoint.trim().to_owned() }
            } else {
                if web_serial_api().is_none() {
                    return Err(web_serial_unsupported_message(self.language).to_owned());
                }
                WebConnectionTarget::Serial
            };
            let target_label = target.label(self.language);

            self.disconnect();
            let worker = WebWorker::spawn(target, self.language);
            self.worker = Some(worker);
            self.connection_state = ConnectionState::Connecting;
            self.print_state = PrintState::Idle;
            self.last_error = None;
            self.firmware_summary = None;
            self.detected_area = None;
            self.selected_port = Some(target_label.clone());
            if self.use_esp3d {
                self.push_log(self.text().trying_to_connect(&target_label));
                if let Some(worker) = self.worker.as_ref() {
                    worker.queue_command(WorkerCommand::QueueManual(initial_probe_commands()));
                }
            } else {
                self.push_log(self.text().opening_browser_port_picker);
            }
            Ok(())
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            let target = if self.use_esp3d {
                NativeConnectionTarget::Esp3d { endpoint: self.esp3d_endpoint.trim().to_owned() }
            } else {
                NativeConnectionTarget::Serial {
                    port_name: self
                        .selected_port
                        .clone()
                        .or_else(|| self.available_ports.first().cloned())
                        .ok_or_else(|| {
                            self.text().select_serial_port_before_connecting.to_owned()
                        })?,
                }
            };
            let target_label = target.label(self.language);

            self.disconnect();

            let (worker, command_tx) = NativeWorker::spawn(target, self.language)?;
            self.worker = Some(worker);
            self.connection_state = ConnectionState::Connecting;
            self.print_state = PrintState::Idle;
            self.last_error = None;
            self.firmware_summary = None;
            self.detected_area = None;
            self.selected_port = Some(target_label.clone());
            self.push_log(self.text().trying_to_connect(&target_label));

            if command_tx.send(WorkerCommand::QueueManual(initial_probe_commands())).is_err() {
                self.worker = None;
                self.connection_state = ConnectionState::Disconnected;
                return Err(self.text().failed_to_start_initial_probe.to_owned());
            }

            Ok(())
        }
    }

    pub fn disconnect(&mut self) {
        #[cfg(target_arch = "wasm32")]
        if let Some(worker) = self.worker.take() {
            worker.queue_command(WorkerCommand::Disconnect);
            self.connection_state = ConnectionState::Disconnected;
            self.print_state = PrintState::Idle;
            self.push_log(self.text().closed_device_connection);
        }

        #[cfg(not(target_arch = "wasm32"))]
        if let Some(worker) = self.worker.take() {
            let _ = worker.command_tx.send(WorkerCommand::Disconnect);
            self.push_log(self.text().closed_device_connection);
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            self.connection_state = ConnectionState::Disconnected;
            self.print_state = PrintState::Idle;
        }
    }

    pub fn send_job(&mut self, gcode_lines: &[String]) -> Result<(), String> {
        #[cfg(target_arch = "wasm32")]
        {
            if self.is_job_active() {
                return Err(self.text().print_already_in_progress.to_owned());
            }

            let worker =
                self.worker.as_ref().ok_or_else(|| self.text().connect_device_first.to_owned())?;
            worker.queue_command(WorkerCommand::QueueJob(gcode_lines.to_vec()));
            self.print_state = PrintState::Printing;
            self.push_log(self.text().queued_gcode_lines(gcode_lines.len()));
            Ok(())
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.is_job_active() {
                return Err(self.text().print_already_in_progress.to_owned());
            }

            let worker =
                self.worker.as_ref().ok_or_else(|| self.text().connect_device_first.to_owned())?;

            worker
                .command_tx
                .send(WorkerCommand::QueueJob(gcode_lines.to_vec()))
                .map_err(|_| self.text().failed_to_queue_gcode_to_device.to_owned())?;

            self.print_state = PrintState::Printing;
            self.push_log(self.text().queued_gcode_lines(gcode_lines.len()));
            Ok(())
        }
    }

    pub fn stop_job(&mut self) -> Result<(), String> {
        #[cfg(target_arch = "wasm32")]
        {
            if !self.can_stop_print() {
                return Err(self.text().no_active_print_job.to_owned());
            }

            let worker =
                self.worker.as_ref().ok_or_else(|| self.text().connect_device_first.to_owned())?;
            worker.queue_command(WorkerCommand::CancelJob);
            self.print_state = PrintState::Stopping;
            self.push_log(self.text().requested_print_stop);
            Ok(())
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            if !self.can_stop_print() {
                return Err(self.text().no_active_print_job.to_owned());
            }

            let worker =
                self.worker.as_ref().ok_or_else(|| self.text().connect_device_first.to_owned())?;
            worker
                .command_tx
                .send(WorkerCommand::CancelJob)
                .map_err(|_| self.text().failed_to_send_stop_command.to_owned())?;

            self.print_state = PrintState::Stopping;
            self.push_log(self.text().requested_print_stop);
            Ok(())
        }
    }

    pub fn jog_xy(
        &mut self,
        delta_x_mm: f32,
        delta_y_mm: f32,
        feed_rate_mm_min: f32,
    ) -> Result<(), String> {
        let command = build_relative_move_command(delta_x_mm, delta_y_mm, 0.0, feed_rate_mm_min)
            .ok_or_else(|| self.text().no_axis_to_move.to_owned())?;
        self.queue_manual_commands(
            self.text().sent_manual_xy_move,
            vec![
                "G21".to_owned(),
                "M400".to_owned(),
                "G91".to_owned(),
                command,
                "M400".to_owned(),
                "G90".to_owned(),
            ],
        )
    }

    pub fn jog_z(&mut self, delta_z_mm: f32, feed_rate_mm_min: f32) -> Result<(), String> {
        let command = build_relative_move_command(0.0, 0.0, delta_z_mm, feed_rate_mm_min)
            .ok_or_else(|| self.text().no_axis_to_move.to_owned())?;
        self.queue_manual_commands(
            self.text().sent_manual_z_move,
            vec![
                "G21".to_owned(),
                "M400".to_owned(),
                "G91".to_owned(),
                command,
                "M400".to_owned(),
                "G90".to_owned(),
            ],
        )
    }

    pub fn home_xy(&mut self) -> Result<(), String> {
        self.queue_manual_commands(
            self.text().sent_xy_home,
            vec!["G21".to_owned(), "M400".to_owned(), "G28 X Y".to_owned()],
        )
    }

    pub fn home_z(&mut self) -> Result<(), String> {
        self.queue_manual_commands(
            self.text().sent_z_home,
            vec!["G21".to_owned(), "M400".to_owned(), "G28 Z".to_owned()],
        )
    }

    pub fn home_xy_and_move_to(
        &mut self,
        x_mm: f32,
        y_mm: f32,
        lift_height_mm: f32,
        feed_rate_mm_min: f32,
    ) -> Result<(), String> {
        let lift_command = build_relative_move_command(0.0, 0.0, lift_height_mm, feed_rate_mm_min)
            .ok_or_else(|| self.text().no_z_lift_distance.to_owned())?;
        self.queue_manual_commands(
            self.text().sent_move_to_first_start,
            vec![
                "G21".to_owned(),
                "M400".to_owned(),
                "G91".to_owned(),
                lift_command,
                "M400".to_owned(),
                "G90".to_owned(),
                "G28 X Y".to_owned(),
                build_absolute_xy_move_command(x_mm, y_mm, feed_rate_mm_min),
                "M400".to_owned(),
            ],
        )
    }

    pub fn move_to(&mut self, x_mm: f32, y_mm: f32, feed_rate_mm_min: f32) -> Result<(), String> {
        let command = build_absolute_xy_move_command(x_mm, y_mm, feed_rate_mm_min);
        self.queue_manual_commands(
            self.text().sent_absolute_move,
            vec!["G21".to_owned(), "M400".to_owned(), "G90".to_owned(), command, "M400".to_owned()],
        )
    }

    pub fn tick(&mut self) -> Option<PrintableArea> {
        #[cfg(target_arch = "wasm32")]
        {
            let mut events = Vec::new();
            {
                let Some(worker) = self.worker.as_ref() else {
                    return None;
                };
                events.extend(worker.drain_events());
            }

            let mut newly_detected_area = None;
            for event in events {
                match event {
                    WorkerEvent::PortOpened => {
                        if self.use_esp3d {
                            self.push_log(self.text().esp3d_http_ready);
                        } else {
                            self.push_log(self.text().opened_serial_port_waiting_firmware);
                        }
                    }
                    WorkerEvent::Connected => {
                        self.connection_state = ConnectionState::Connected;
                    }
                    WorkerEvent::ReadyTimeout => {
                        self.last_error = Some(ready_timeout_message(self.language).to_owned());
                        self.connection_state = ConnectionState::Disconnected;
                        self.print_state = PrintState::Idle;
                        self.push_log(
                            self.text().device_error(ready_timeout_message(self.language)),
                        );
                        self.worker = None;
                        break;
                    }
                    WorkerEvent::Line(line) => {
                        if let Some(firmware) = parse_firmware(&line) {
                            self.firmware_summary = Some(firmware);
                        }
                        if let Some(area) = detect_build_volume(&line) {
                            self.detected_area = Some(area);
                            newly_detected_area = Some(area);
                        }
                        self.push_log(line);
                    }
                    WorkerEvent::Error(message) => {
                        self.last_error = Some(message.clone());
                        self.connection_state = ConnectionState::Disconnected;
                        self.print_state = PrintState::Idle;
                        self.push_log(self.text().device_error(&message));
                        self.worker = None;
                        break;
                    }
                    WorkerEvent::JobCompleted => {
                        self.print_state = PrintState::Idle;
                        self.push_log(self.text().printing_completed);
                    }
                    WorkerEvent::JobCancelled => {
                        self.print_state = PrintState::Idle;
                        self.push_log(self.text().printing_stopped);
                    }
                    WorkerEvent::Disconnected => {
                        self.connection_state = ConnectionState::Disconnected;
                        self.print_state = PrintState::Idle;
                        self.worker = None;
                        break;
                    }
                }
            }
            return newly_detected_area;
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut events = Vec::new();

            {
                let Some(worker) = self.worker.as_ref() else {
                    return None;
                };

                while let Ok(event) = worker.event_rx.try_recv() {
                    events.push(event);
                }
            }

            let mut newly_detected_area = None;

            for event in events {
                match event {
                    WorkerEvent::PortOpened => {
                        if self.use_esp3d {
                            self.push_log(self.text().esp3d_http_ready);
                        } else {
                            self.push_log(self.text().opened_serial_port_waiting_firmware);
                        }
                    }
                    WorkerEvent::Connected => {
                        self.connection_state = ConnectionState::Connected;
                    }
                    WorkerEvent::ReadyTimeout => {
                        self.last_error = Some(ready_timeout_message(self.language).to_owned());
                        self.connection_state = ConnectionState::Disconnected;
                        self.print_state = PrintState::Idle;
                        self.push_log(
                            self.text().device_error(ready_timeout_message(self.language)),
                        );
                        self.worker = None;
                        break;
                    }
                    WorkerEvent::Line(line) => {
                        if let Some(firmware) = parse_firmware(&line) {
                            self.firmware_summary = Some(firmware);
                        }

                        if let Some(area) = detect_build_volume(&line) {
                            self.detected_area = Some(area);
                            newly_detected_area = Some(area);
                        }

                        self.push_log(line);
                    }
                    WorkerEvent::Error(message) => {
                        self.last_error = Some(message.clone());
                        self.connection_state = ConnectionState::Disconnected;
                        self.print_state = PrintState::Idle;
                        self.push_log(self.text().device_error(&message));
                        self.worker = None;
                        break;
                    }
                    WorkerEvent::JobCompleted => {
                        self.print_state = PrintState::Idle;
                        self.push_log(self.text().printing_completed);
                    }
                    WorkerEvent::JobCancelled => {
                        self.print_state = PrintState::Idle;
                        self.push_log(self.text().printing_stopped);
                    }
                    WorkerEvent::Disconnected => {
                        self.connection_state = ConnectionState::Disconnected;
                        self.print_state = PrintState::Idle;
                        self.worker = None;
                        break;
                    }
                }
            }

            newly_detected_area
        }
    }

    fn push_log(&mut self, line: impl Into<String>) {
        self.log.push_back(line.into());
        while self.log.len() > DEVICE_LOG_LIMIT {
            self.log.pop_front();
        }
    }

    fn queue_manual_commands(
        &mut self,
        log_line: &str,
        commands: Vec<String>,
    ) -> Result<(), String> {
        #[cfg(target_arch = "wasm32")]
        {
            if self.is_job_active() {
                return Err(self.text().manual_control_unavailable_while_printing.to_owned());
            }

            let worker =
                self.worker.as_ref().ok_or_else(|| self.text().connect_device_first.to_owned())?;
            worker.queue_command(WorkerCommand::QueueManual(commands));
            self.push_log(log_line);
            Ok(())
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.is_job_active() {
                return Err(self.text().manual_control_unavailable_while_printing.to_owned());
            }

            let worker =
                self.worker.as_ref().ok_or_else(|| self.text().connect_device_first.to_owned())?;
            worker
                .command_tx
                .send(WorkerCommand::QueueManual(commands))
                .map_err(|_| self.text().failed_to_send_manual_control_command.to_owned())?;
            self.push_log(log_line);
            Ok(())
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
struct NativeWorker {
    command_tx: std::sync::mpsc::Sender<WorkerCommand>,
    event_rx: std::sync::mpsc::Receiver<WorkerEvent>,
}

#[cfg(not(target_arch = "wasm32"))]
enum NativeConnectionTarget {
    Serial { port_name: String },
    Esp3d { endpoint: String },
}

#[cfg(not(target_arch = "wasm32"))]
impl NativeConnectionTarget {
    fn label(&self, language: Language) -> String {
        match self {
            Self::Serial { port_name } => port_name.clone(),
            Self::Esp3d { endpoint } => esp3d_target_label(endpoint, language),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl NativeWorker {
    fn spawn(
        target: NativeConnectionTarget,
        language: Language,
    ) -> Result<(Self, std::sync::mpsc::Sender<WorkerCommand>), String> {
        let (command_tx, command_rx) = std::sync::mpsc::channel();
        let (event_tx, event_rx) = std::sync::mpsc::channel();
        let thread_command_tx = command_tx.clone();
        let thread_name =
            format!("penartic-device-{}", sanitize_thread_name(&target.label(language)));

        std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || run_worker(target, command_rx, event_tx, language))
            .map_err(|error| error.to_string())?;

        Ok((Self { command_tx: command_tx.clone(), event_rx }, thread_command_tx))
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn sanitize_thread_name(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() || ch == '-' { ch } else { '_' })
        .take(48)
        .collect()
}

enum WorkerCommand {
    QueueJob(Vec<String>),
    QueueManual(Vec<String>),
    CancelJob,
    Disconnect,
}

enum WorkerEvent {
    PortOpened,
    Connected,
    ReadyTimeout,
    Line(String),
    JobCompleted,
    JobCancelled,
    Error(String),
    Disconnected,
}

#[cfg(not(target_arch = "wasm32"))]
fn run_worker(
    target: NativeConnectionTarget,
    command_rx: std::sync::mpsc::Receiver<WorkerCommand>,
    event_tx: std::sync::mpsc::Sender<WorkerEvent>,
    language: Language,
) {
    match target {
        NativeConnectionTarget::Serial { port_name } => {
            run_serial_worker(port_name, command_rx, event_tx, language)
        }
        NativeConnectionTarget::Esp3d { endpoint } => {
            run_esp3d_worker(endpoint, command_rx, event_tx, language)
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn run_serial_worker(
    port_name: String,
    command_rx: std::sync::mpsc::Receiver<WorkerCommand>,
    event_tx: std::sync::mpsc::Sender<WorkerEvent>,
    language: Language,
) {
    use std::io::Read as _;
    use std::time::Instant;

    let result = (|| -> Result<(), String> {
        let mut port = serialport::new(&port_name, 115_200)
            .timeout(Duration::from_millis(100))
            .open()
            .map_err(|error| error.to_string())?;
        port.write_data_terminal_ready(true).map_err(|error| error.to_string())?;
        port.write_request_to_send(true).map_err(|error| error.to_string())?;

        event_tx.send(WorkerEvent::PortOpened).map_err(|error| error.to_string())?;

        let mut queued_lines: VecDeque<QueuedLine> = VecDeque::new();
        let mut queued_job_count = 0usize;
        let mut in_flight_lines = VecDeque::new();
        let mut in_flight_job_count = 0usize;
        let mut job_cancelled = false;
        let mut read_buffer = [0_u8; 512];
        let mut pending_text = String::new();
        let mut ready = false;
        let ready_started_at = Instant::now();
        let mut last_ready_ping_at: Option<Instant> = None;

        loop {
            while let Ok(command) = command_rx.try_recv() {
                match command {
                    WorkerCommand::QueueJob(lines) => {
                        let lines = clean_gcode_lines(lines);
                        if !lines.is_empty() {
                            job_cancelled = false;
                        }
                        queued_job_count += lines.len();
                        queued_lines.extend(
                            lines
                                .into_iter()
                                .map(|line| QueuedLine { line, source: QueuedLineSource::Job }),
                        );
                    }
                    WorkerCommand::QueueManual(lines) => {
                        let lines = clean_gcode_lines(lines);
                        queued_lines.extend(
                            lines
                                .into_iter()
                                .map(|line| QueuedLine { line, source: QueuedLineSource::Manual }),
                        );
                    }
                    WorkerCommand::CancelJob => {
                        job_cancelled = true;
                        queued_lines = queued_lines
                            .into_iter()
                            .filter(|queued| queued.source != QueuedLineSource::Job)
                            .collect();
                        queued_job_count = 0;
                        queued_lines.push_front(QueuedLine {
                            line: "M400".to_owned(),
                            source: QueuedLineSource::Stop,
                        });
                        queued_lines.push_front(QueuedLine {
                            line: "M410".to_owned(),
                            source: QueuedLineSource::Stop,
                        });
                        if in_flight_job_count == 0 {
                            event_tx
                                .send(WorkerEvent::JobCancelled)
                                .map_err(|error| error.to_string())?;
                            job_cancelled = false;
                        }
                    }
                    WorkerCommand::Disconnect => return Ok(()),
                }
            }

            if !ready {
                let now = Instant::now();
                if last_ready_ping_at
                    .is_none_or(|last| now.duration_since(last) >= READY_PING_INTERVAL)
                {
                    port.write_all(b"M115\n").map_err(|error| error.to_string())?;
                    port.flush().map_err(|error| error.to_string())?;
                    last_ready_ping_at = Some(now);
                }

                if now.duration_since(ready_started_at) >= READY_TIMEOUT {
                    event_tx.send(WorkerEvent::ReadyTimeout).map_err(|error| error.to_string())?;
                    return Ok(());
                }
            }

            while ready && in_flight_lines.len() < MAX_IN_FLIGHT_LINES {
                let mut batch = Vec::new();

                while in_flight_lines.len() < MAX_IN_FLIGHT_LINES {
                    let Some(queued) = queued_lines.pop_front() else {
                        break;
                    };

                    batch.extend_from_slice(queued.line.as_bytes());
                    batch.push(b'\n');

                    if queued.source == QueuedLineSource::Job {
                        queued_job_count = queued_job_count.saturating_sub(1);
                        in_flight_job_count += 1;
                    }
                    in_flight_lines.push_back(queued);

                    if batch.len() >= 2048 {
                        break;
                    }
                }

                if batch.is_empty() {
                    break;
                }

                port.write_all(&batch).map_err(|error| error.to_string())?;
                port.flush().map_err(|error| error.to_string())?;
            }

            match port.read(&mut read_buffer) {
                Ok(bytes_read) if bytes_read > 0 => {
                    pending_text.push_str(&String::from_utf8_lossy(&read_buffer[..bytes_read]));

                    while let Some(end_of_line) = pending_text.find('\n') {
                        let line = pending_text[..end_of_line].trim().to_owned();
                        pending_text.drain(..=end_of_line);

                        if line.is_empty() {
                            continue;
                        }

                        if !ready && is_ready_line(&line) {
                            ready = true;
                            event_tx
                                .send(WorkerEvent::Connected)
                                .map_err(|error| error.to_string())?;
                        }

                        if ready && is_ack_line(&line) {
                            if let Some(acknowledged) = in_flight_lines.pop_front() {
                                if acknowledged.source == QueuedLineSource::Job {
                                    in_flight_job_count = in_flight_job_count.saturating_sub(1);
                                    if queued_job_count == 0 && in_flight_job_count == 0 {
                                        event_tx
                                            .send(if job_cancelled {
                                                WorkerEvent::JobCancelled
                                            } else {
                                                WorkerEvent::JobCompleted
                                            })
                                            .map_err(|error| error.to_string())?;
                                        job_cancelled = false;
                                    }
                                }
                            }
                        }

                        let line = annotate_busy_line(line, in_flight_lines.front(), language);
                        event_tx
                            .send(WorkerEvent::Line(line))
                            .map_err(|error| error.to_string())?;
                    }
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => return Err(error.to_string()),
            }
        }
    })();

    if let Err(error) = result {
        let _ = event_tx.send(WorkerEvent::Error(error));
    }

    let _ = event_tx.send(WorkerEvent::Disconnected);
}

#[cfg(not(target_arch = "wasm32"))]
fn run_esp3d_worker(
    endpoint: String,
    command_rx: std::sync::mpsc::Receiver<WorkerCommand>,
    event_tx: std::sync::mpsc::Sender<WorkerEvent>,
    language: Language,
) {
    if esp3d_websocket_available(&endpoint, language) {
        run_esp3d_websocket_worker(endpoint, command_rx, event_tx, language);
    } else {
        run_esp3d_http_worker(endpoint, command_rx, event_tx, language);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn run_esp3d_websocket_worker(
    endpoint: String,
    command_rx: std::sync::mpsc::Receiver<WorkerCommand>,
    event_tx: std::sync::mpsc::Sender<WorkerEvent>,
    language: Language,
) {
    use std::io::ErrorKind;

    use tungstenite::{Message, client::IntoClientRequest as _, stream::MaybeTlsStream};

    let result = (|| -> Result<(), String> {
        let endpoint = normalize_esp3d_endpoint(&endpoint)?;
        let mut request = endpoint.clone().into_client_request().map_err(|error| {
            format!("{}: {error}", language.strings().esp3d_http_request_failed)
        })?;
        request.headers_mut().insert("Sec-WebSocket-Protocol", "arduino".parse().unwrap());
        let (mut socket, _) = tungstenite::connect(request).map_err(|error| {
            format!("{}: {error}", language.strings().esp3d_http_request_failed)
        })?;
        if let MaybeTlsStream::Plain(stream) = socket.get_mut() {
            stream.set_read_timeout(Some(Duration::from_millis(100))).map_err(|error| {
                format!("{}: {error}", language.strings().esp3d_http_request_failed)
            })?;
        }

        event_tx.send(WorkerEvent::PortOpened).map_err(|error| error.to_string())?;
        event_tx.send(WorkerEvent::Connected).map_err(|error| error.to_string())?;
        event_tx
            .send(WorkerEvent::Line(language.strings().esp3d_http_connected.to_owned()))
            .map_err(|error| error.to_string())?;

        let mut queued_lines: VecDeque<QueuedLine> = VecDeque::new();
        let mut queued_job_count = 0usize;
        let mut in_flight_lines = VecDeque::new();
        let mut in_flight_job_count = 0usize;
        let mut job_active = false;
        let mut job_cancelled = false;

        loop {
            while let Ok(command) = command_rx.try_recv() {
                match command {
                    WorkerCommand::QueueJob(lines) => {
                        let lines = clean_gcode_lines(lines);
                        if !lines.is_empty() {
                            job_active = true;
                            job_cancelled = false;
                        }
                        queued_job_count += lines.len();
                        queued_lines.extend(
                            lines
                                .into_iter()
                                .map(|line| QueuedLine { line, source: QueuedLineSource::Job }),
                        );
                    }
                    WorkerCommand::QueueManual(lines) => {
                        queued_lines.extend(
                            clean_gcode_lines(lines)
                                .into_iter()
                                .map(|line| QueuedLine { line, source: QueuedLineSource::Manual }),
                        );
                    }
                    WorkerCommand::CancelJob => {
                        job_cancelled = true;
                        queued_lines = queued_lines
                            .into_iter()
                            .filter(|queued| queued.source != QueuedLineSource::Job)
                            .collect();
                        queued_job_count = 0;
                        send_esp3d_websocket_line(
                            &mut socket,
                            QueuedLine { line: "M410".to_owned(), source: QueuedLineSource::Stop },
                            &mut in_flight_lines,
                            language,
                        )?;
                        send_esp3d_websocket_line(
                            &mut socket,
                            QueuedLine { line: "M400".to_owned(), source: QueuedLineSource::Stop },
                            &mut in_flight_lines,
                            language,
                        )?;
                        if in_flight_job_count == 0 {
                            event_tx
                                .send(WorkerEvent::JobCancelled)
                                .map_err(|error| error.to_string())?;
                            job_cancelled = false;
                        }
                    }
                    WorkerCommand::Disconnect => return Ok(()),
                }
            }

            top_up_esp3d_websocket_queue(
                &mut socket,
                &mut queued_lines,
                &mut queued_job_count,
                &mut in_flight_lines,
                &mut in_flight_job_count,
                language,
            )?;

            match socket.read() {
                Ok(Message::Text(text)) => {
                    handle_esp3d_websocket_text(
                        text.as_str(),
                        &mut in_flight_lines,
                        &mut in_flight_job_count,
                        &queued_job_count,
                        &mut job_active,
                        &mut job_cancelled,
                        &event_tx,
                        language,
                    )?;
                }
                Ok(Message::Binary(bytes)) => {
                    let text = String::from_utf8_lossy(&bytes);
                    handle_esp3d_websocket_text(
                        &text,
                        &mut in_flight_lines,
                        &mut in_flight_job_count,
                        &queued_job_count,
                        &mut job_active,
                        &mut job_cancelled,
                        &event_tx,
                        language,
                    )?;
                }
                Ok(Message::Ping(payload)) => {
                    socket.send(Message::Pong(payload)).map_err(|error| {
                        format!("{}: {error}", language.strings().esp3d_http_request_failed)
                    })?;
                }
                Ok(Message::Close(_)) => return Ok(()),
                Ok(_) => {}
                Err(tungstenite::Error::Io(error)) if error.kind() == ErrorKind::WouldBlock => {}
                Err(tungstenite::Error::Io(error)) if error.kind() == ErrorKind::TimedOut => {}
                Err(error) => {
                    return Err(format!(
                        "{}: {error}",
                        language.strings().esp3d_http_request_failed
                    ));
                }
            }
        }
    })();

    if let Err(error) = result {
        let _ = event_tx.send(WorkerEvent::Error(error));
    }

    let _ = event_tx.send(WorkerEvent::Disconnected);
}

#[cfg(not(target_arch = "wasm32"))]
fn handle_esp3d_websocket_text(
    text: &str,
    in_flight_lines: &mut VecDeque<QueuedLine>,
    in_flight_job_count: &mut usize,
    queued_job_count: &usize,
    job_active: &mut bool,
    job_cancelled: &mut bool,
    event_tx: &std::sync::mpsc::Sender<WorkerEvent>,
    language: Language,
) -> Result<(), String> {
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        for _ in 0..ack_response_count(line) {
            acknowledge_queued_line(
                in_flight_lines,
                in_flight_job_count,
                *queued_job_count,
                job_active,
                job_cancelled,
                event_tx,
            )?;
        }

        let line = annotate_busy_line(line.to_owned(), in_flight_lines.front(), language);
        event_tx.send(WorkerEvent::Line(line)).map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn send_esp3d_websocket_line(
    socket: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
    queued: QueuedLine,
    in_flight_lines: &mut VecDeque<QueuedLine>,
    language: Language,
) -> Result<(), String> {
    let line = format!("{}\n", queued.line);
    socket
        .send(tungstenite::Message::Text(line.into()))
        .map_err(|error| format!("{}: {error}", language.strings().esp3d_http_request_failed))?;
    in_flight_lines.push_back(queued);
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn top_up_esp3d_websocket_queue(
    socket: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
    queued_lines: &mut VecDeque<QueuedLine>,
    queued_job_count: &mut usize,
    in_flight_lines: &mut VecDeque<QueuedLine>,
    in_flight_job_count: &mut usize,
    language: Language,
) -> Result<(), String> {
    let mut sent_this_tick = 0usize;
    while in_flight_lines.len() < ESP3D_MAX_IN_FLIGHT_LINES
        && sent_this_tick < ESP3D_TOP_UP_LINES_PER_TICK
    {
        let Some(queued) = queued_lines.pop_front() else {
            break;
        };
        if queued.source == QueuedLineSource::Job {
            *queued_job_count = queued_job_count.saturating_sub(1);
            *in_flight_job_count += 1;
        }
        send_esp3d_websocket_line(socket, queued, in_flight_lines, language)?;
        sent_this_tick += 1;
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn acknowledge_queued_line(
    in_flight_lines: &mut VecDeque<QueuedLine>,
    in_flight_job_count: &mut usize,
    queued_job_count: usize,
    job_active: &mut bool,
    job_cancelled: &mut bool,
    event_tx: &std::sync::mpsc::Sender<WorkerEvent>,
) -> Result<(), String> {
    let Some(acknowledged) = in_flight_lines.pop_front() else {
        return Ok(());
    };

    if acknowledged.source == QueuedLineSource::Job {
        *in_flight_job_count = (*in_flight_job_count).saturating_sub(1);
    }

    if *job_active && queued_job_count == 0 && *in_flight_job_count == 0 {
        let has_in_flight_job =
            in_flight_lines.iter().any(|line| line.source == QueuedLineSource::Job);
        if !has_in_flight_job {
            event_tx
                .send(if *job_cancelled {
                    WorkerEvent::JobCancelled
                } else {
                    WorkerEvent::JobCompleted
                })
                .map_err(|error| error.to_string())?;
            *job_active = false;
            *job_cancelled = false;
        }
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn esp3d_websocket_available(endpoint: &str, language: Language) -> bool {
    use tungstenite::client::IntoClientRequest as _;

    let Ok(endpoint) = normalize_esp3d_endpoint(endpoint) else {
        return false;
    };
    let Ok(mut request) = endpoint.into_client_request() else {
        return false;
    };
    request.headers_mut().insert("Sec-WebSocket-Protocol", "arduino".parse().unwrap());
    match tungstenite::connect(request) {
        Ok((mut socket, _)) => {
            let _ = socket.close(None);
            true
        }
        Err(error) => {
            log::debug!("{}: {error}", language.strings().esp3d_http_request_failed);
            false
        }
    }
}

fn normalize_esp3d_endpoint(endpoint: &str) -> Result<String, String> {
    let endpoint = endpoint.trim();
    if endpoint.is_empty() {
        return Err("ESP3D endpoint is empty.".to_owned());
    }
    if endpoint.starts_with("ws://") || endpoint.starts_with("wss://") {
        return Ok(endpoint.to_owned());
    }

    let (scheme, rest) = if let Some(rest) = endpoint.strip_prefix("http://") {
        ("ws", rest)
    } else if let Some(rest) = endpoint.strip_prefix("https://") {
        ("wss", rest)
    } else {
        ("ws", endpoint)
    };
    let host_port = rest.split('/').next().unwrap_or(rest).trim_end_matches('/');
    if host_port.is_empty() {
        return Err("ESP3D endpoint host is empty.".to_owned());
    }

    let websocket_host_port = if let Some((host, port)) = host_port.rsplit_once(':') {
        match port.parse::<u16>() {
            Ok(80 | 443) => format!("{host}:{ESP3D_DEFAULT_DATA_WEBSOCKET_PORT}"),
            Ok(_) => format!("{host}:{port}"),
            Err(_) => format!("{host_port}:{ESP3D_DEFAULT_DATA_WEBSOCKET_PORT}"),
        }
    } else {
        format!("{host_port}:{ESP3D_DEFAULT_DATA_WEBSOCKET_PORT}")
    };
    Ok(format!("{scheme}://{websocket_host_port}/"))
}

#[cfg(not(target_arch = "wasm32"))]
fn run_esp3d_http_worker(
    endpoint: String,
    command_rx: std::sync::mpsc::Receiver<WorkerCommand>,
    event_tx: std::sync::mpsc::Sender<WorkerEvent>,
    language: Language,
) {
    let result = (|| -> Result<(), String> {
        let endpoint = normalize_esp3d_http_endpoint(&endpoint)?;
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|error| error.to_string())?;

        event_tx.send(WorkerEvent::PortOpened).map_err(|error| error.to_string())?;
        for line in send_esp3d_http_command(&client, &endpoint, "M115", language)? {
            event_tx.send(WorkerEvent::Line(line)).map_err(|error| error.to_string())?;
        }
        event_tx.send(WorkerEvent::Connected).map_err(|error| error.to_string())?;

        let mut queued_lines: VecDeque<QueuedLine> = VecDeque::new();
        let mut queued_job_count = 0usize;
        let mut job_cancelled = false;

        loop {
            while let Ok(command) = command_rx.try_recv() {
                match command {
                    WorkerCommand::QueueJob(lines) => {
                        let lines = clean_gcode_lines(lines);
                        if !lines.is_empty() {
                            job_cancelled = false;
                        }
                        queued_job_count += lines.len();
                        queued_lines.extend(
                            lines
                                .into_iter()
                                .map(|line| QueuedLine { line, source: QueuedLineSource::Job }),
                        );
                    }
                    WorkerCommand::QueueManual(lines) => {
                        queued_lines.extend(
                            clean_gcode_lines(lines)
                                .into_iter()
                                .map(|line| QueuedLine { line, source: QueuedLineSource::Manual }),
                        );
                    }
                    WorkerCommand::CancelJob => {
                        job_cancelled = true;
                        queued_lines = queued_lines
                            .into_iter()
                            .filter(|queued| queued.source != QueuedLineSource::Job)
                            .collect();
                        queued_job_count = 0;
                        queued_lines.push_front(QueuedLine {
                            line: "M400".to_owned(),
                            source: QueuedLineSource::Stop,
                        });
                        queued_lines.push_front(QueuedLine {
                            line: "M410".to_owned(),
                            source: QueuedLineSource::Stop,
                        });
                        event_tx
                            .send(WorkerEvent::JobCancelled)
                            .map_err(|error| error.to_string())?;
                    }
                    WorkerCommand::Disconnect => return Ok(()),
                }
            }

            if let Some(queued) = queued_lines.pop_front() {
                for line in send_esp3d_http_command(&client, &endpoint, &queued.line, language)? {
                    event_tx.send(WorkerEvent::Line(line)).map_err(|error| error.to_string())?;
                }

                if queued.source == QueuedLineSource::Job {
                    queued_job_count = queued_job_count.saturating_sub(1);
                    if queued_job_count == 0 {
                        event_tx
                            .send(if job_cancelled {
                                WorkerEvent::JobCancelled
                            } else {
                                WorkerEvent::JobCompleted
                            })
                            .map_err(|error| error.to_string())?;
                        job_cancelled = false;
                    }
                }
            } else {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    })();

    if let Err(error) = result {
        let _ = event_tx.send(WorkerEvent::Error(error));
    }

    let _ = event_tx.send(WorkerEvent::Disconnected);
}

#[cfg(not(target_arch = "wasm32"))]
fn normalize_esp3d_http_endpoint(endpoint: &str) -> Result<String, String> {
    let endpoint = endpoint.trim();
    if endpoint.is_empty() {
        return Err("ESP3D endpoint is empty.".to_owned());
    }
    let without_websocket_scheme =
        endpoint.strip_prefix("ws://").or_else(|| endpoint.strip_prefix("wss://"));
    let with_scheme = if let Some(rest) = without_websocket_scheme {
        let host_port = rest.split('/').next().unwrap_or(rest).trim_end_matches('/');
        let http_host = if let Some((host, port)) = host_port.rsplit_once(':') {
            if port.parse::<u16>().ok() == Some(ESP3D_DEFAULT_DATA_WEBSOCKET_PORT) {
                host
            } else {
                host_port
            }
        } else {
            host_port
        };
        format!("http://{http_host}")
    } else if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        endpoint.to_owned()
    } else {
        format!("http://{endpoint}")
    };
    Ok(with_scheme.trim_end_matches('/').to_owned())
}

#[cfg(not(target_arch = "wasm32"))]
fn send_esp3d_http_command(
    client: &reqwest::blocking::Client,
    endpoint: &str,
    command: &str,
    language: Language,
) -> Result<Vec<String>, String> {
    let response = match send_esp3d_http_command_request(client, endpoint, "cmd", command, language)
    {
        Ok(response) if response.status().is_success() => response,
        _ => send_esp3d_http_command_request(client, endpoint, "commandText", command, language)?,
    };
    let status = response.status();
    if !status.is_success() {
        return Err(format!("{}: HTTP {status}", language.strings().esp3d_http_request_failed));
    }

    let body = response
        .text()
        .map_err(|error| format!("{}: {error}", language.strings().esp3d_http_request_failed))?;
    let mut lines: Vec<String> = body
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    if lines.is_empty() {
        lines.push(format!("ESP3D: {command}"));
    }
    Ok(lines)
}

#[cfg(not(target_arch = "wasm32"))]
fn send_esp3d_http_command_request(
    client: &reqwest::blocking::Client,
    endpoint: &str,
    parameter: &str,
    command: &str,
    language: Language,
) -> Result<reqwest::blocking::Response, String> {
    let url = format!("{endpoint}/command");
    client
        .get(url)
        .query(&[(parameter, command)])
        .send()
        .map_err(|error| format!("{}: {error}", language.strings().esp3d_http_request_failed))
}

fn parse_firmware(line: &str) -> Option<String> {
    let upper = line.to_ascii_uppercase();
    if upper.contains("FIRMWARE_NAME") || upper.contains("MACHINE_TYPE") {
        Some(line.to_owned())
    } else {
        None
    }
}

fn is_ack_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower == "ok" || lower.starts_with("ok ")
}

fn ack_response_count(text: &str) -> usize {
    let lower = text.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut index = 0usize;
    let mut count = 0usize;

    while index + 1 < bytes.len() {
        if bytes[index] != b'o' || bytes[index + 1] != b'k' {
            index += 1;
            continue;
        }

        let before_boundary = index == 0
            || !bytes[index - 1].is_ascii_alphanumeric()
            || (index >= 2 && bytes[index - 2] == b'o' && bytes[index - 1] == b'k');
        let after = bytes.get(index + 2).copied();
        let after_boundary = after.is_none_or(|byte| !byte.is_ascii_alphanumeric() || byte == b'o');

        if before_boundary && after_boundary {
            count += 1;
            index += 2;
        } else {
            index += 1;
        }
    }

    count
}

fn is_ready_line(line: &str) -> bool {
    let upper = line.to_ascii_uppercase();
    is_ack_line(line) || upper.contains("FIRMWARE_NAME") || upper == "START"
}

fn is_busy_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("busy:") || lower.starts_with("echo:busy")
}

fn annotate_busy_line(
    line: String,
    waiting_line: Option<&QueuedLine>,
    language: Language,
) -> String {
    let text = language.strings();
    if !is_busy_line(&line) {
        return line;
    }

    match waiting_line {
        Some(waiting) => text.busy_waiting_command(&line, &waiting.line),
        None => line,
    }
}

fn clean_gcode_lines(lines: Vec<String>) -> Vec<String> {
    lines
        .into_iter()
        .filter_map(|line| {
            let command = line.split(';').next().unwrap_or_default().trim();
            (!command.is_empty()).then(|| command.to_owned())
        })
        .collect()
}

fn initial_probe_commands() -> Vec<String> {
    vec!["M115".to_owned(), "M503".to_owned(), "M211".to_owned()]
}

fn esp3d_target_label(endpoint: &str, language: Language) -> String {
    format!("{}: {endpoint}", language.strings().esp3d_device)
}

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
fn browser_port_selection_label(language: Language) -> &'static str {
    language.strings().select_port_in_browser
}

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
fn web_serial_device_label(language: Language) -> &'static str {
    language.strings().web_serial_device
}

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
fn web_serial_unsupported_message(language: Language) -> &'static str {
    language.strings().web_serial_unsupported
}

fn ready_timeout_message(language: Language) -> &'static str {
    language.strings().firmware_ready_timeout
}

#[cfg(target_arch = "wasm32")]
fn relabel_port_entry(
    entry: &mut Option<String>,
    previous: Language,
    next: Language,
    label: fn(Language) -> &'static str,
) {
    if let Some(value) = entry.as_mut() {
        relabel_port_value(value, previous, next, label);
    }
}

#[cfg(target_arch = "wasm32")]
fn relabel_esp3d_target_entry(entry: &mut Option<String>, previous: Language, next: Language) {
    let Some(value) = entry.as_mut() else {
        return;
    };
    let previous_prefix = format!("{}: ", previous.strings().esp3d_device);
    let next_prefix = format!("{}: ", next.strings().esp3d_device);
    if value.starts_with(&previous_prefix) {
        *value = format!("{next_prefix}{}", &value[previous_prefix.len()..]);
    } else if value.starts_with(&next_prefix) {
        *value = format!("{next_prefix}{}", &value[next_prefix.len()..]);
    }
}

#[cfg(target_arch = "wasm32")]
fn relabel_port_value(
    value: &mut String,
    previous: Language,
    next: Language,
    label: fn(Language) -> &'static str,
) {
    if *value == label(previous) || *value == label(next) {
        *value = label(next).to_owned();
    }
}

fn detect_build_volume(line: &str) -> Option<PrintableArea> {
    let upper = line.to_ascii_uppercase();
    if upper.contains("MIN:") && upper.contains("MAX:") {
        return detect_min_max_build_volume(&upper);
    }

    let looks_like_size_line = upper.contains("M208")
        || upper.contains("BED")
        || upper.contains("BUILD")
        || upper.contains("VOLUME");

    if !looks_like_size_line {
        return None;
    }

    let x = extract_axis_value(&upper, 'X')?;
    let y = extract_axis_value(&upper, 'Y')?;

    if x > 10.0 && y > 10.0 { Some(PrintableArea::new(x, y)) } else { None }
}

fn detect_min_max_build_volume(line: &str) -> Option<PrintableArea> {
    let max_start = line.find("MAX:")?;
    let max_values = &line[max_start + "MAX:".len()..];
    let max_x = extract_axis_value(max_values, 'X')?;
    let max_y = extract_axis_value(max_values, 'Y')?;

    let min_values = line
        .find("MIN:")
        .map(|min_start| &line[min_start + "MIN:".len()..max_start])
        .unwrap_or_default();
    let min_x = extract_axis_value(min_values, 'X').unwrap_or(0.0);
    let min_y = extract_axis_value(min_values, 'Y').unwrap_or(0.0);

    let width = max_x - min_x;
    let height = max_y - min_y;
    if width > 10.0 && height > 10.0 { Some(PrintableArea::new(width, height)) } else { None }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum QueuedLineSource {
    Job,
    Manual,
    Stop,
}

struct QueuedLine {
    line: String,
    source: QueuedLineSource,
}

fn build_relative_move_command(
    delta_x_mm: f32,
    delta_y_mm: f32,
    delta_z_mm: f32,
    feed_rate_mm_min: f32,
) -> Option<String> {
    let mut command = "G1".to_owned();

    if delta_x_mm.abs() > f32::EPSILON {
        command.push_str(&format!(" X{delta_x_mm:.3}"));
    }
    if delta_y_mm.abs() > f32::EPSILON {
        command.push_str(&format!(" Y{delta_y_mm:.3}"));
    }
    if delta_z_mm.abs() > f32::EPSILON {
        command.push_str(&format!(" Z{delta_z_mm:.3}"));
    }
    if command == "G0" {
        return None;
    }

    command.push_str(&format!(" F{:.0}", feed_rate_mm_min.max(60.0)));
    Some(command)
}

fn build_absolute_xy_move_command(x_mm: f32, y_mm: f32, feed_rate_mm_min: f32) -> String {
    format!("G1 X{x_mm:.2} Y{y_mm:.2} F{:.0}", feed_rate_mm_min.max(60.0))
}

fn extract_axis_value(line: &str, axis: char) -> Option<f32> {
    let bytes = line.as_bytes();
    let axis = axis as u8;
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] != axis {
            index += 1;
            continue;
        }

        let mut cursor = index + 1;
        while cursor < bytes.len() && matches!(bytes[cursor], b'-' | b' ' | b':' | b'=') {
            cursor += 1;
        }

        let start = cursor;
        while cursor < bytes.len()
            && (bytes[cursor].is_ascii_digit() || matches!(bytes[cursor], b'.' | b'-'))
        {
            cursor += 1;
        }

        if start != cursor {
            if let Ok(value) = line[start..cursor].parse::<f32>() {
                return Some(value);
            }
        }

        index = cursor;
    }

    None
}

#[cfg(target_arch = "wasm32")]
#[derive(Clone, Default)]
struct WebEventQueue(Rc<RefCell<VecDeque<WorkerEvent>>>);

#[cfg(target_arch = "wasm32")]
impl WebEventQueue {
    fn push(&self, event: WorkerEvent) {
        self.0.borrow_mut().push_back(event);
    }

    fn drain(&self) -> Vec<WorkerEvent> {
        self.0.borrow_mut().drain(..).collect()
    }
}

#[cfg(target_arch = "wasm32")]
#[derive(Clone, Default)]
struct WebCommandQueue(Rc<RefCell<VecDeque<WorkerCommand>>>);

#[cfg(target_arch = "wasm32")]
impl WebCommandQueue {
    fn push(&self, command: WorkerCommand) {
        self.0.borrow_mut().push_back(command);
    }

    fn pop(&self) -> Option<WorkerCommand> {
        self.0.borrow_mut().pop_front()
    }
}

#[cfg(target_arch = "wasm32")]
struct WebWorker {
    commands: WebCommandQueue,
    events: WebEventQueue,
}

#[cfg(target_arch = "wasm32")]
enum WebConnectionTarget {
    Serial,
    Esp3d { endpoint: String },
}

#[cfg(target_arch = "wasm32")]
impl WebWorker {
    fn spawn(target: WebConnectionTarget, language: Language) -> Self {
        let commands = WebCommandQueue::default();
        let events = WebEventQueue::default();
        match target {
            WebConnectionTarget::Serial => {
                spawn_local(run_web_serial_worker(commands.clone(), events.clone(), language));
            }
            WebConnectionTarget::Esp3d { endpoint } => {
                spawn_local(run_websocket_worker(
                    commands.clone(),
                    events.clone(),
                    endpoint,
                    language,
                ));
            }
        }
        Self { commands, events }
    }

    fn queue_command(&self, command: WorkerCommand) {
        self.commands.push(command);
    }

    fn drain_events(&self) -> Vec<WorkerEvent> {
        self.events.drain()
    }
}

#[cfg(target_arch = "wasm32")]
impl WebConnectionTarget {
    fn label(&self, language: Language) -> String {
        match self {
            Self::Serial => web_serial_device_label(language).to_owned(),
            Self::Esp3d { endpoint } => esp3d_target_label(endpoint, language),
        }
    }
}

#[cfg(target_arch = "wasm32")]
async fn run_web_serial_worker(
    commands: WebCommandQueue,
    events: WebEventQueue,
    language: Language,
) {
    let result = run_web_serial_worker_inner(commands, events.clone(), language).await;
    if let Err(error) = result {
        events.push(WorkerEvent::Error(error));
    }
    events.push(WorkerEvent::Disconnected);
}

#[cfg(target_arch = "wasm32")]
async fn run_web_serial_worker_inner(
    commands: WebCommandQueue,
    events: WebEventQueue,
    language: Language,
) -> Result<(), String> {
    let serial =
        web_serial_api().ok_or_else(|| web_serial_unsupported_message(language).to_owned())?;
    let port = await_js(call_method0(&serial, "requestPort")?).await?;
    let options = Object::new();
    Reflect::set(&options, &JsValue::from_str("baudRate"), &JsValue::from_f64(115_200.0))
        .map_err(js_error_message)?;
    await_js(call_method1(&port, "open", options.as_ref())?).await?;
    events.push(WorkerEvent::PortOpened);

    let readable = Reflect::get(&port, &JsValue::from_str("readable")).map_err(js_error_message)?;
    let reader = call_method0(&readable, "getReader")?;
    let writable = Reflect::get(&port, &JsValue::from_str("writable")).map_err(js_error_message)?;
    let writer = call_method0(&writable, "getWriter")?;

    let shared = WebSerialShared::new(commands, events.clone(), writer, reader.clone(), language);
    spawn_local(web_writer_loop(shared.clone()));

    let mut pending_text = String::new();
    loop {
        let read_result = await_js(call_method0(&reader, "read")?).await?;
        let done = Reflect::get(&read_result, &JsValue::from_str("done"))
            .map_err(js_error_message)?
            .as_bool()
            .unwrap_or(false);
        if done {
            break;
        }

        let value =
            Reflect::get(&read_result, &JsValue::from_str("value")).map_err(js_error_message)?;
        if value.is_undefined() || value.is_null() {
            continue;
        }

        let bytes = Uint8Array::new(&value).to_vec();
        pending_text.push_str(&String::from_utf8_lossy(&bytes));
        while let Some(end_of_line) = pending_text.find('\n') {
            let line = pending_text[..end_of_line].trim().to_owned();
            pending_text.drain(..=end_of_line);
            if line.is_empty() {
                continue;
            }
            shared.handle_line(&line);
        }

        if shared.disconnect_requested() {
            break;
        }
    }

    shared.request_disconnect();
    let _ = call_method0(&reader, "releaseLock");
    let _ = call_method0(&port, "close").and_then(|promise| promise_to_future(promise));
    Ok(())
}

#[cfg(target_arch = "wasm32")]
#[derive(Clone)]
struct WebSerialShared(Rc<RefCell<WebSerialState>>);

#[cfg(target_arch = "wasm32")]
struct WebSerialState {
    commands: WebCommandQueue,
    events: WebEventQueue,
    language: Language,
    writer: JsValue,
    reader: JsValue,
    queued_lines: VecDeque<QueuedLine>,
    queued_job_count: usize,
    in_flight_lines: VecDeque<QueuedLine>,
    in_flight_job_count: usize,
    job_active: bool,
    job_cancelled: bool,
    ready: bool,
    disconnect_requested: bool,
}

#[cfg(target_arch = "wasm32")]
impl WebSerialShared {
    fn new(
        commands: WebCommandQueue,
        events: WebEventQueue,
        writer: JsValue,
        reader: JsValue,
        language: Language,
    ) -> Self {
        Self(Rc::new(RefCell::new(WebSerialState {
            commands,
            events,
            language,
            writer,
            reader,
            queued_lines: VecDeque::new(),
            queued_job_count: 0,
            in_flight_lines: VecDeque::new(),
            in_flight_job_count: 0,
            job_active: false,
            job_cancelled: false,
            ready: false,
            disconnect_requested: false,
        })))
    }

    fn request_disconnect(&self) {
        self.0.borrow_mut().disconnect_requested = true;
    }

    fn disconnect_requested(&self) -> bool {
        self.0.borrow().disconnect_requested
    }

    fn handle_line(&self, line: &str) {
        let mut state = self.0.borrow_mut();
        if !state.ready && is_ready_line(line) {
            state.ready = true;
            state.queued_lines.extend(initial_probe_commands().into_iter().map(|line| {
                QueuedLine { line: line.to_owned(), source: QueuedLineSource::Manual }
            }));
            state.events.push(WorkerEvent::Connected);
        }

        if state.ready && is_ack_line(line) {
            let queued_job_count = state.queued_job_count;
            let WebSerialState {
                in_flight_lines,
                in_flight_job_count,
                job_active,
                job_cancelled,
                events,
                ..
            } = &mut *state;
            acknowledge_web_queued_line(
                in_flight_lines,
                in_flight_job_count,
                queued_job_count,
                job_active,
                job_cancelled,
                events,
            );
        }

        state.events.push(WorkerEvent::Line(annotate_busy_line(
            line.to_owned(),
            state.in_flight_lines.front(),
            state.language,
        )));
    }
}

#[cfg(target_arch = "wasm32")]
async fn web_writer_loop(shared: WebSerialShared) {
    let mut last_ready_ping_ms = 0.0;
    let ready_started_ms = js_sys::Date::now();

    loop {
        if shared.disconnect_requested() {
            return;
        }

        let write_result = {
            let mut state = shared.0.borrow_mut();

            while let Some(command) = state.commands.pop() {
                match command {
                    WorkerCommand::QueueJob(lines) => {
                        let lines = clean_gcode_lines(lines);
                        if !lines.is_empty() {
                            state.job_active = true;
                            state.job_cancelled = false;
                        }
                        state.queued_job_count += lines.len();
                        state.queued_lines.extend(
                            lines
                                .into_iter()
                                .map(|line| QueuedLine { line, source: QueuedLineSource::Job }),
                        );
                    }
                    WorkerCommand::QueueManual(lines) => {
                        state.queued_lines.extend(
                            clean_gcode_lines(lines)
                                .into_iter()
                                .map(|line| QueuedLine { line, source: QueuedLineSource::Manual }),
                        );
                    }
                    WorkerCommand::CancelJob => {
                        state.job_cancelled = true;
                        state.queued_lines = state
                            .queued_lines
                            .drain(..)
                            .filter(|queued| queued.source != QueuedLineSource::Job)
                            .collect();
                        state.queued_job_count = 0;
                        state.queued_lines.push_front(QueuedLine {
                            line: "M400".to_owned(),
                            source: QueuedLineSource::Stop,
                        });
                        state.queued_lines.push_front(QueuedLine {
                            line: "M410".to_owned(),
                            source: QueuedLineSource::Stop,
                        });
                        if state.in_flight_job_count == 0 {
                            state.events.push(WorkerEvent::JobCancelled);
                            state.job_active = false;
                            state.job_cancelled = false;
                        }
                    }
                    WorkerCommand::Disconnect => {
                        state.disconnect_requested = true;
                        let _ = call_method0(&state.reader, "cancel");
                        return;
                    }
                }
            }

            if !state.ready {
                let now = js_sys::Date::now();
                if now - last_ready_ping_ms >= READY_PING_INTERVAL.as_millis() as f64 {
                    last_ready_ping_ms = now;
                    Some((state.writer.clone(), b"M115\n".to_vec()))
                } else if now - ready_started_ms >= READY_TIMEOUT.as_millis() as f64 {
                    state.events.push(WorkerEvent::ReadyTimeout);
                    state.disconnect_requested = true;
                    None
                } else {
                    None
                }
            } else if state.in_flight_lines.len() < MAX_IN_FLIGHT_LINES {
                let mut batch = Vec::new();
                while state.in_flight_lines.len() < MAX_IN_FLIGHT_LINES {
                    let Some(queued) = state.queued_lines.pop_front() else {
                        break;
                    };

                    batch.extend_from_slice(queued.line.as_bytes());
                    batch.push(b'\n');
                    if queued.source == QueuedLineSource::Job {
                        state.queued_job_count = state.queued_job_count.saturating_sub(1);
                        state.in_flight_job_count += 1;
                    }
                    state.in_flight_lines.push_back(queued);

                    if batch.len() >= 2048 {
                        break;
                    }
                }

                (!batch.is_empty()).then(|| (state.writer.clone(), batch))
            } else {
                None
            }
        };

        if let Some((writer, bytes)) = write_result {
            if let Err(error) = web_serial_write(&writer, &bytes).await {
                let mut state = shared.0.borrow_mut();
                state.events.push(WorkerEvent::Error(error));
                state.disconnect_requested = true;
                let _ = call_method0(&state.reader, "cancel");
            }
        }

        delay_ms(10).await;
    }
}

#[cfg(target_arch = "wasm32")]
async fn run_websocket_worker(
    commands: WebCommandQueue,
    events: WebEventQueue,
    endpoint: String,
    language: Language,
) {
    let result = run_websocket_worker_inner(commands, events.clone(), endpoint, language).await;
    if let Err(error) = result {
        events.push(WorkerEvent::Error(error));
    }
    events.push(WorkerEvent::Disconnected);
}

#[cfg(target_arch = "wasm32")]
async fn run_websocket_worker_inner(
    commands: WebCommandQueue,
    events: WebEventQueue,
    endpoint: String,
    language: Language,
) -> Result<(), String> {
    let endpoint = normalize_esp3d_endpoint(&endpoint)?;
    let socket = WebSocket::new_with_str(&endpoint, "arduino")
        .map_err(|error| websocket_connect_error(&endpoint, error, language))?;
    socket.set_binary_type(BinaryType::Arraybuffer);

    let shared = WebSocketShared::new(commands, events, language);

    let open_shared = shared.clone();
    let open_language = language;
    let on_open = Closure::wrap(Box::new(move |_event: Event| {
        let mut state = open_shared.0.borrow_mut();
        if state.disconnect_requested || state.ready {
            return;
        }

        state.ready = true;
        state.events.push(WorkerEvent::PortOpened);
        state.events.push(WorkerEvent::Connected);
        state
            .events
            .push(WorkerEvent::Line(open_language.strings().esp3d_http_connected.to_owned()));
    }) as Box<dyn FnMut(_)>);
    socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));

    let message_shared = shared.clone();
    let on_message = Closure::wrap(Box::new(move |event: MessageEvent| {
        if let Some(text) = event.data().as_string() {
            message_shared.handle_text(&text);
            return;
        }

        if let Ok(buffer) = event.data().dyn_into::<ArrayBuffer>() {
            let bytes = Uint8Array::new(&buffer).to_vec();
            let text = String::from_utf8_lossy(&bytes);
            message_shared.handle_text(&text);
        }
    }) as Box<dyn FnMut(_)>);
    socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));

    let error_shared = shared.clone();
    let error_language = language;
    let on_error = Closure::wrap(Box::new(move |_event: Event| {
        let mut state = error_shared.0.borrow_mut();
        if state.disconnect_requested {
            return;
        }

        state.disconnect_requested = true;
        state.events.push(WorkerEvent::Error(
            error_language.strings().esp3d_http_request_failed.to_owned(),
        ));
    }) as Box<dyn FnMut(_)>);
    socket.set_onerror(Some(on_error.as_ref().unchecked_ref()));

    let close_shared = shared.clone();
    let on_close = Closure::wrap(Box::new(move |_event: CloseEvent| {
        close_shared.request_disconnect();
    }) as Box<dyn FnMut(_)>);
    socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));

    spawn_local(websocket_writer_loop(shared.clone(), socket.clone()));

    while !shared.disconnect_requested() {
        delay_ms(10).await;
    }

    socket.set_onopen(None);
    socket.set_onmessage(None);
    socket.set_onerror(None);
    socket.set_onclose(None);
    let _ = socket.close();
    Ok(())
}

#[cfg(target_arch = "wasm32")]
#[derive(Clone)]
struct WebSocketShared(Rc<RefCell<WebSocketState>>);

#[cfg(target_arch = "wasm32")]
struct WebSocketState {
    commands: WebCommandQueue,
    events: WebEventQueue,
    language: Language,
    queued_lines: VecDeque<QueuedLine>,
    queued_job_count: usize,
    in_flight_lines: VecDeque<QueuedLine>,
    in_flight_job_count: usize,
    job_active: bool,
    job_cancelled: bool,
    ready: bool,
    disconnect_requested: bool,
}

#[cfg(target_arch = "wasm32")]
impl WebSocketShared {
    fn new(commands: WebCommandQueue, events: WebEventQueue, language: Language) -> Self {
        Self(Rc::new(RefCell::new(WebSocketState {
            commands,
            events,
            language,
            queued_lines: VecDeque::new(),
            queued_job_count: 0,
            in_flight_lines: VecDeque::new(),
            in_flight_job_count: 0,
            job_active: false,
            job_cancelled: false,
            ready: false,
            disconnect_requested: false,
        })))
    }

    fn request_disconnect(&self) {
        self.0.borrow_mut().disconnect_requested = true;
    }

    fn disconnect_requested(&self) -> bool {
        self.0.borrow().disconnect_requested
    }

    fn handle_text(&self, text: &str) {
        let mut state = self.0.borrow_mut();
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
            for _ in 0..ack_response_count(line) {
                let queued_job_count = state.queued_job_count;
                let WebSocketState {
                    in_flight_lines,
                    in_flight_job_count,
                    job_active,
                    job_cancelled,
                    events,
                    ..
                } = &mut *state;
                acknowledge_web_queued_line(
                    in_flight_lines,
                    in_flight_job_count,
                    queued_job_count,
                    job_active,
                    job_cancelled,
                    events,
                );
            }

            state.events.push(WorkerEvent::Line(annotate_busy_line(
                line.to_owned(),
                state.in_flight_lines.front(),
                state.language,
            )));
        }
    }
}

#[cfg(target_arch = "wasm32")]
async fn websocket_writer_loop(shared: WebSocketShared, socket: WebSocket) {
    loop {
        if shared.disconnect_requested() {
            let _ = socket.close();
            return;
        }

        let mut send_error = None;
        {
            let mut state = shared.0.borrow_mut();

            while send_error.is_none() {
                let Some(command) = state.commands.pop() else {
                    break;
                };

                match command {
                    WorkerCommand::QueueJob(lines) => {
                        let lines = clean_gcode_lines(lines);
                        if !lines.is_empty() {
                            state.job_active = true;
                            state.job_cancelled = false;
                        }
                        state.queued_job_count += lines.len();
                        state.queued_lines.extend(
                            lines
                                .into_iter()
                                .map(|line| QueuedLine { line, source: QueuedLineSource::Job }),
                        );
                    }
                    WorkerCommand::QueueManual(lines) => {
                        state.queued_lines.extend(
                            clean_gcode_lines(lines)
                                .into_iter()
                                .map(|line| QueuedLine { line, source: QueuedLineSource::Manual }),
                        );
                    }
                    WorkerCommand::CancelJob => {
                        state.job_cancelled = true;
                        state.queued_lines = state
                            .queued_lines
                            .drain(..)
                            .filter(|queued| queued.source != QueuedLineSource::Job)
                            .collect();
                        state.queued_job_count = 0;
                        if state.ready {
                            let language = state.language;
                            let stop_line = QueuedLine {
                                line: "M410".to_owned(),
                                source: QueuedLineSource::Stop,
                            };
                            if let Err(error) = websocket_send_line(
                                &socket,
                                stop_line,
                                &mut state.in_flight_lines,
                                language,
                            ) {
                                send_error = Some(error);
                            }
                            if send_error.is_none() {
                                let stop_line = QueuedLine {
                                    line: "M400".to_owned(),
                                    source: QueuedLineSource::Stop,
                                };
                                if let Err(error) = websocket_send_line(
                                    &socket,
                                    stop_line,
                                    &mut state.in_flight_lines,
                                    language,
                                ) {
                                    send_error = Some(error);
                                }
                            }
                        } else {
                            state.queued_lines.push_front(QueuedLine {
                                line: "M400".to_owned(),
                                source: QueuedLineSource::Stop,
                            });
                            state.queued_lines.push_front(QueuedLine {
                                line: "M410".to_owned(),
                                source: QueuedLineSource::Stop,
                            });
                        }
                        if state.in_flight_job_count == 0 {
                            state.events.push(WorkerEvent::JobCancelled);
                            state.job_active = false;
                            state.job_cancelled = false;
                        }
                    }
                    WorkerCommand::Disconnect => {
                        state.disconnect_requested = true;
                        break;
                    }
                }
            }

            if send_error.is_none() && !state.disconnect_requested && state.ready {
                let language = state.language;
                let WebSocketState {
                    queued_lines,
                    queued_job_count,
                    in_flight_lines,
                    in_flight_job_count,
                    ..
                } = &mut *state;
                if let Err(error) = top_up_websocket_queue(
                    &socket,
                    queued_lines,
                    queued_job_count,
                    in_flight_lines,
                    in_flight_job_count,
                    language,
                ) {
                    send_error = Some(error);
                }
            }
        }

        if let Some(error) = send_error {
            let mut state = shared.0.borrow_mut();
            state.events.push(WorkerEvent::Error(error));
            state.disconnect_requested = true;
        }

        delay_ms(10).await;
    }
}

#[cfg(target_arch = "wasm32")]
fn acknowledge_web_queued_line(
    in_flight_lines: &mut VecDeque<QueuedLine>,
    in_flight_job_count: &mut usize,
    queued_job_count: usize,
    job_active: &mut bool,
    job_cancelled: &mut bool,
    events: &WebEventQueue,
) {
    let Some(acknowledged) = in_flight_lines.pop_front() else {
        return;
    };

    if acknowledged.source == QueuedLineSource::Job {
        *in_flight_job_count = (*in_flight_job_count).saturating_sub(1);
    }

    if *job_active && queued_job_count == 0 && *in_flight_job_count == 0 {
        let has_in_flight_job =
            in_flight_lines.iter().any(|line| line.source == QueuedLineSource::Job);
        if !has_in_flight_job {
            events.push(if *job_cancelled {
                WorkerEvent::JobCancelled
            } else {
                WorkerEvent::JobCompleted
            });
            *job_active = false;
            *job_cancelled = false;
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn websocket_send_line(
    socket: &WebSocket,
    queued: QueuedLine,
    in_flight_lines: &mut VecDeque<QueuedLine>,
    language: Language,
) -> Result<(), String> {
    socket.send_with_str(&format!("{}\n", queued.line)).map_err(|error| {
        format!("{}: {}", language.strings().esp3d_http_request_failed, js_error_message(error))
    })?;
    in_flight_lines.push_back(queued);
    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn top_up_websocket_queue(
    socket: &WebSocket,
    queued_lines: &mut VecDeque<QueuedLine>,
    queued_job_count: &mut usize,
    in_flight_lines: &mut VecDeque<QueuedLine>,
    in_flight_job_count: &mut usize,
    language: Language,
) -> Result<(), String> {
    let mut sent_this_tick = 0usize;
    while in_flight_lines.len() < ESP3D_MAX_IN_FLIGHT_LINES
        && sent_this_tick < ESP3D_TOP_UP_LINES_PER_TICK
    {
        let Some(queued) = queued_lines.pop_front() else {
            break;
        };
        if queued.source == QueuedLineSource::Job {
            *queued_job_count = queued_job_count.saturating_sub(1);
            *in_flight_job_count += 1;
        }
        websocket_send_line(socket, queued, in_flight_lines, language)?;
        sent_this_tick += 1;
    }
    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn websocket_connect_error(endpoint: &str, error: JsValue, language: Language) -> String {
    let detail = js_error_message(error);
    if endpoint.starts_with("ws://") && web_page_uses_https() {
        format!("{}: {detail}", language.strings().secure_websocket_required)
    } else {
        format!("{}: {detail}", language.strings().esp3d_http_request_failed)
    }
}

#[cfg(target_arch = "wasm32")]
async fn web_serial_write(writer: &JsValue, bytes: &[u8]) -> Result<(), String> {
    let data = Uint8Array::new_with_length(bytes.len() as u32);
    data.copy_from(bytes);
    await_js(call_method1(writer, "write", data.as_ref())?).await?;
    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn web_serial_api() -> Option<JsValue> {
    let window = eframe::web_sys::window()?;
    let navigator = window.navigator();
    let serial = Reflect::get(navigator.as_ref(), &JsValue::from_str("serial")).ok()?;
    (!serial.is_undefined() && !serial.is_null()).then_some(serial)
}

#[cfg(target_arch = "wasm32")]
fn web_page_uses_https() -> bool {
    eframe::web_sys::window().and_then(|window| window.location().protocol().ok()).as_deref()
        == Some("https:")
}

#[cfg(target_arch = "wasm32")]
fn call_method0(target: &JsValue, name: &str) -> Result<JsValue, String> {
    Reflect::get(target, &JsValue::from_str(name))
        .map_err(js_error_message)?
        .dyn_into::<Function>()
        .map_err(js_error_message)?
        .call0(target)
        .map_err(js_error_message)
}

#[cfg(target_arch = "wasm32")]
fn call_method1(target: &JsValue, name: &str, arg: &JsValue) -> Result<JsValue, String> {
    Reflect::get(target, &JsValue::from_str(name))
        .map_err(js_error_message)?
        .dyn_into::<Function>()
        .map_err(js_error_message)?
        .call1(target, arg)
        .map_err(js_error_message)
}

#[cfg(target_arch = "wasm32")]
fn promise_to_future(value: JsValue) -> Result<JsFuture, String> {
    value.dyn_into::<js_sys::Promise>().map(JsFuture::from).map_err(js_error_message)
}

#[cfg(target_arch = "wasm32")]
async fn await_js(value: JsValue) -> Result<JsValue, String> {
    promise_to_future(value)?.await.map_err(js_error_message)
}

#[cfg(target_arch = "wasm32")]
async fn delay_ms(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        if let Some(window) = eframe::web_sys::window() {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
        } else {
            let _ = resolve.call0(&JsValue::UNDEFINED);
        }
    });
    let _ = JsFuture::from(promise).await;
}

#[cfg(target_arch = "wasm32")]
fn js_error_message(value: JsValue) -> String {
    if let Some(message) = Reflect::get(&value, &JsValue::from_str("message"))
        .ok()
        .and_then(|message| message.as_string())
    {
        message
    } else if let Some(text) = value.as_string() {
        text
    } else {
        format!("{value:?}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_m208_size_report() {
        let size = detect_build_volume("echo:  M208 X220.00 Y220.00 Z250.00 S0").unwrap();
        assert_eq!(size.width_mm, 220.0);
        assert_eq!(size.height_mm, 220.0);
    }

    #[test]
    fn parses_marlin_m211_min_max_report() {
        let size =
            detect_build_volume("Min:  X0.00 Y0.00 Z0.00   Max:  X235.00 Y235.00 Z250.00").unwrap();
        assert_eq!(size.width_mm, 235.0);
        assert_eq!(size.height_mm, 235.0);
    }

    #[test]
    fn removes_comments_before_sending_gcode() {
        let lines = clean_gcode_lines(vec![
            "; Generated by Penartic".to_owned(),
            "G1 X1 ; inline comment".to_owned(),
            "  ".to_owned(),
        ]);
        assert_eq!(lines, vec!["G1 X1"]);
    }

    #[test]
    fn builds_relative_xy_move_command() {
        let command = build_relative_move_command(10.0, -2.5, 0.0, 1800.0).unwrap();
        assert_eq!(command, "G1 X10.000 Y-2.500 F1800");
    }

    #[test]
    fn builds_absolute_xy_move_command_for_first_point() {
        let command = build_absolute_xy_move_command(12.5, 4.0, 1800.0);
        assert_eq!(command, "G1 X12.50 Y4.00 F1800");
    }

    #[test]
    fn normalizes_http_esp3d_address_to_data_websocket_port() {
        assert_eq!(
            normalize_esp3d_endpoint("http://192.168.0.112/").unwrap(),
            "ws://192.168.0.112:8282/"
        );
        assert_eq!(
            normalize_esp3d_endpoint("http://192.168.0.112:80/").unwrap(),
            "ws://192.168.0.112:8282/"
        );
        assert_eq!(
            normalize_esp3d_endpoint("ws://192.168.0.112:8282/").unwrap(),
            "ws://192.168.0.112:8282/"
        );
        assert_eq!(
            normalize_esp3d_endpoint("https://esp3d.local/").unwrap(),
            "wss://esp3d.local:8282/"
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn esp3d_completion_survives_batched_ack_frames() {
        let mut in_flight = VecDeque::from([
            QueuedLine { line: "M400".to_owned(), source: QueuedLineSource::Manual },
            QueuedLine { line: "G1 X1".to_owned(), source: QueuedLineSource::Job },
        ]);
        let mut in_flight_job_count = 1;
        let queued_job_count = 0;
        let mut job_active = true;
        let mut job_cancelled = false;
        let (event_tx, event_rx) = std::sync::mpsc::channel();

        handle_esp3d_websocket_text(
            "ok\nok\n",
            &mut in_flight,
            &mut in_flight_job_count,
            &queued_job_count,
            &mut job_active,
            &mut job_cancelled,
            &event_tx,
            Language::English,
        )
        .unwrap();

        assert_eq!(in_flight_job_count, 0);
        assert!(!job_active);
        assert!(matches!(event_rx.try_recv(), Ok(WorkerEvent::Line(_))));
        assert!(matches!(event_rx.try_recv(), Ok(WorkerEvent::JobCompleted)));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn esp3d_completion_handles_compacted_ack_frame() {
        let mut in_flight = VecDeque::from([
            QueuedLine { line: "G1 X1".to_owned(), source: QueuedLineSource::Job },
            QueuedLine { line: "M400".to_owned(), source: QueuedLineSource::Job },
        ]);
        let mut in_flight_job_count = 2;
        let queued_job_count = 0;
        let mut job_active = true;
        let mut job_cancelled = false;
        let (event_tx, event_rx) = std::sync::mpsc::channel();

        handle_esp3d_websocket_text(
            "okok",
            &mut in_flight,
            &mut in_flight_job_count,
            &queued_job_count,
            &mut job_active,
            &mut job_cancelled,
            &event_tx,
            Language::English,
        )
        .unwrap();

        assert_eq!(in_flight_job_count, 0);
        assert!(!job_active);
        assert!(matches!(event_rx.try_recv(), Ok(WorkerEvent::JobCompleted)));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn counts_esp3d_ack_responses_without_matching_words() {
        assert_eq!(ack_response_count("ok"), 1);
        assert_eq!(ack_response_count("ok N0 P15 B15"), 1);
        assert_eq!(ack_response_count("okok"), 2);
        assert_eq!(ack_response_count("ok\r\nok"), 2);
        assert_eq!(ack_response_count("look"), 0);
        assert_eq!(ack_response_count("okay"), 0);
    }
}
