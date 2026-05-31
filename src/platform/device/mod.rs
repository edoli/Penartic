mod esp3d;
mod octoprint;
mod serial;

use std::{collections::VecDeque, time::Duration};

use serde::{Deserialize, Serialize};

use crate::{plot::model::PrintableArea, res::lang::Language};

#[cfg(target_arch = "wasm32")]
use std::{cell::RefCell, rc::Rc};

#[cfg(target_arch = "wasm32")]
use js_sys::{Function, Reflect, Uint8Array};

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::{JsCast, JsValue};

#[cfg(target_arch = "wasm32")]
use wasm_bindgen_futures::{JsFuture, spawn_local};

const DEVICE_LOG_LIMIT: usize = 200;
const DEFAULT_ESP3D_ENDPOINT: &str = "http://192.168.0.112/";
const DEFAULT_OCTOPRINT_BASE_URL: &str = "http://127.0.0.1:5000/";
pub(super) const MAX_IN_FLIGHT_LINES: usize = 4;
pub(super) const READY_PING_INTERVAL: Duration = Duration::from_millis(500);
pub(super) const READY_TIMEOUT: Duration = Duration::from_secs(15);
pub(super) const ESP3D_MAX_IN_FLIGHT_LINES: usize = 16;
pub(super) const ESP3D_TOP_UP_LINES_PER_TICK: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrintState {
    Idle,
    Printing,
    Stopping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionMethod {
    #[default]
    Serial,
    Esp3d,
    OctoPrint,
}

impl ConnectionMethod {
    pub fn available() -> &'static [Self] {
        &[Self::Serial, Self::Esp3d, Self::OctoPrint]
    }

    pub fn label(self, language: Language) -> &'static str {
        let text = language.strings();
        match self {
            Self::Serial => text.connection_method_serial,
            Self::Esp3d => text.connection_method_esp3d,
            Self::OctoPrint => text.connection_method_octoprint,
        }
    }

    fn is_supported_on_current_platform(self) -> bool {
        Self::available().contains(&self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DevicePreferences {
    pub connection_method: ConnectionMethod,
    pub selected_serial_port: Option<String>,
    pub esp3d_endpoint: String,
    pub octoprint_base_url: String,
    pub octoprint_api_key: String,
}

impl Default for DevicePreferences {
    fn default() -> Self {
        Self {
            connection_method: ConnectionMethod::Serial,
            selected_serial_port: None,
            esp3d_endpoint: DEFAULT_ESP3D_ENDPOINT.to_owned(),
            octoprint_base_url: DEFAULT_OCTOPRINT_BASE_URL.to_owned(),
            octoprint_api_key: String::new(),
        }
    }
}

impl DevicePreferences {
    fn sanitized(mut self) -> Self {
        self.selected_serial_port = self
            .selected_serial_port
            .take()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        self.esp3d_endpoint = self.esp3d_endpoint.trim().to_owned();
        self.octoprint_base_url = self.octoprint_base_url.trim().to_owned();
        self.octoprint_api_key = self.octoprint_api_key.trim().to_owned();

        if !self.connection_method.is_supported_on_current_platform() {
            self.connection_method =
                ConnectionMethod::available().first().copied().unwrap_or(ConnectionMethod::Serial);
        }

        self
    }
}

pub struct DeviceController {
    language: Language,
    preferences: DevicePreferences,
    available_ports: Vec<String>,
    connected_target_label: Option<String>,
    connection_state: ConnectionState,
    print_state: PrintState,
    firmware_summary: Option<String>,
    detected_area: Option<PrintableArea>,
    log: VecDeque<String>,
    last_error: Option<String>,
    print_progress: Option<f32>,
    #[cfg(not(target_arch = "wasm32"))]
    worker: Option<NativeWorker>,
    #[cfg(target_arch = "wasm32")]
    worker: Option<WebWorker>,
}

impl DeviceController {
    pub fn new(language: Language, preferences: DevicePreferences) -> Self {
        Self {
            language,
            preferences: preferences.sanitized(),
            available_ports: Vec::new(),
            connected_target_label: None,
            connection_state: ConnectionState::Disconnected,
            print_state: PrintState::Idle,
            firmware_summary: None,
            detected_area: None,
            log: VecDeque::new(),
            last_error: None,
            print_progress: None,
            #[cfg(not(target_arch = "wasm32"))]
            worker: None,
            #[cfg(target_arch = "wasm32")]
            worker: None,
        }
    }

    fn text(&self) -> &'static crate::res::lang::Strings {
        self.language.strings()
    }

    pub fn set_language(&mut self, language: Language) {
        #[cfg(target_arch = "wasm32")]
        {
            let previous = self.language;
            relabel_port_entry(
                &mut self.preferences.selected_serial_port,
                previous,
                language,
                browser_port_selection_label,
            );
            relabel_port_entry(
                &mut self.connected_target_label,
                previous,
                language,
                web_serial_device_label,
            );
            for port in &mut self.available_ports {
                relabel_port_value(port, previous, language, browser_port_selection_label);
            }
        }

        self.language = language;
    }

    pub fn preferences(&self) -> DevicePreferences {
        self.preferences.clone().sanitized()
    }

    pub fn connection_method(&self) -> ConnectionMethod {
        self.preferences.connection_method
    }

    pub fn set_connection_method(&mut self, method: ConnectionMethod) {
        if method.is_supported_on_current_platform() {
            self.preferences.connection_method = method;
            self.last_error = None;
        }
    }

    pub fn serial_ports(&self) -> &[String] {
        &self.available_ports
    }

    pub fn selected_serial_port(&self) -> Option<&str> {
        self.preferences.selected_serial_port.as_deref()
    }

    pub fn set_selected_serial_port(&mut self, selected_port: Option<String>) {
        self.preferences.selected_serial_port =
            selected_port.map(|value| value.trim().to_owned()).filter(|value| !value.is_empty());
    }

    pub fn esp3d_endpoint(&self) -> &str {
        &self.preferences.esp3d_endpoint
    }

    pub fn set_esp3d_endpoint(&mut self, endpoint: String) {
        self.preferences.esp3d_endpoint = endpoint;
    }

    pub fn octoprint_base_url(&self) -> &str {
        &self.preferences.octoprint_base_url
    }

    pub fn set_octoprint_base_url(&mut self, base_url: String) {
        self.preferences.octoprint_base_url = base_url;
    }

    pub fn octoprint_api_key(&self) -> &str {
        &self.preferences.octoprint_api_key
    }

    pub fn set_octoprint_api_key(&mut self, api_key: String) {
        self.preferences.octoprint_api_key = api_key;
    }

    pub fn connection_state(&self) -> ConnectionState {
        self.connection_state
    }

    pub fn print_state(&self) -> PrintState {
        self.print_state
    }

    pub fn print_progress(&self) -> Option<f32> {
        self.print_progress
    }

    pub fn firmware_summary(&self) -> Option<&str> {
        self.firmware_summary.as_deref()
    }

    pub fn detected_area(&self) -> Option<PrintableArea> {
        self.detected_area
    }

    pub fn log_lines(&self) -> impl DoubleEndedIterator<Item = &str> + '_ {
        self.log.iter().map(String::as_str)
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    fn set_print_progress(&mut self, progress: Option<f32>) {
        self.print_progress = progress.map(|value| value.clamp(0.0, 1.0));
    }

    pub fn refresh_serial_ports(&mut self) {
        #[cfg(target_arch = "wasm32")]
        {
            if web_serial_api().is_some() {
                self.available_ports = vec![browser_port_selection_label(self.language).to_owned()];
                if self.preferences.selected_serial_port.is_none() {
                    self.preferences.selected_serial_port = self.available_ports.first().cloned();
                }
            } else {
                self.available_ports.clear();
                self.preferences.selected_serial_port = None;
                self.push_log(web_serial_unsupported_message(self.language).to_owned());
            }
            return;
        }

        #[cfg(not(target_arch = "wasm32"))]
        match serialport::available_ports() {
            Ok(ports) => {
                self.available_ports = ports.into_iter().map(|port| port.port_name).collect();
                self.available_ports.sort();
                if self.preferences.selected_serial_port.as_deref().is_none_or(|selected| {
                    !self.available_ports.iter().any(|port| port == selected)
                }) {
                    self.preferences.selected_serial_port = self.available_ports.first().cloned();
                }
                self.push_log(self.text().found_serial_ports(self.available_ports.len()));
            }
            Err(error) => {
                self.available_ports.clear();
                self.preferences.selected_serial_port = None;
                self.push_log(self.text().failed_to_read_port_list(error));
            }
        }
    }

    pub fn refresh_ports(&mut self) {
        self.refresh_serial_ports();
    }

    pub fn needs_poll(&self) -> bool {
        matches!(self.connection_state, ConnectionState::Connecting)
            || matches!(self.print_state, PrintState::Printing | PrintState::Stopping)
    }

    pub fn status_text(&self) -> String {
        match self.connection_state {
            ConnectionState::Disconnected => self.text().disconnected.to_owned(),
            ConnectionState::Connecting => self.text().connecting.to_owned(),
            ConnectionState::Connected => self
                .connected_target_label
                .as_deref()
                .map(|target| self.text().connected_status(target))
                .unwrap_or_else(|| self.text().connected.to_owned()),
            ConnectionState::Unsupported => self.text().web_preview_only.to_owned(),
        }
    }

    pub fn print_state_text(&self) -> &'static str {
        let text = self.text();
        match self.print_state {
            PrintState::Idle => text.idle,
            PrintState::Printing => text.printing,
            PrintState::Stopping => text.stopping,
        }
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
        if self.connection_state != ConnectionState::Disconnected {
            return false;
        }

        #[cfg(target_arch = "wasm32")]
        {
            match self.preferences.connection_method {
                ConnectionMethod::Serial => web_serial_api().is_some(),
                ConnectionMethod::Esp3d => !self.preferences.esp3d_endpoint.trim().is_empty(),
                ConnectionMethod::OctoPrint => {
                    !self.preferences.octoprint_base_url.trim().is_empty()
                }
            }
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            match self.preferences.connection_method {
                ConnectionMethod::Serial => !self.available_ports.is_empty(),
                ConnectionMethod::Esp3d => !self.preferences.esp3d_endpoint.trim().is_empty(),
                ConnectionMethod::OctoPrint => {
                    !self.preferences.octoprint_base_url.trim().is_empty()
                }
            }
        }
    }

    pub fn connect(&mut self) -> Result<(), String> {
        if !self.can_connect() {
            return Err(self.text().connect_device_first.to_owned());
        }

        #[cfg(target_arch = "wasm32")]
        {
            let target = match self.preferences.connection_method {
                ConnectionMethod::Serial => {
                    if web_serial_api().is_none() {
                        return Err(web_serial_unsupported_message(self.language).to_owned());
                    }
                    WebConnectionTarget::Serial
                }
                ConnectionMethod::Esp3d => WebConnectionTarget::Esp3d {
                    endpoint: self.preferences.esp3d_endpoint.trim().to_owned(),
                },
                ConnectionMethod::OctoPrint => WebConnectionTarget::OctoPrint {
                    base_url: self.preferences.octoprint_base_url.trim().to_owned(),
                    api_key: self.preferences.octoprint_api_key.trim().to_owned(),
                },
            };
            let target_label = target.target_label(self.language);

            self.disconnect();
            let worker = WebWorker::spawn(target, self.language);
            self.worker = Some(worker);
            self.connection_state = ConnectionState::Connecting;
            self.print_state = PrintState::Idle;
            self.set_print_progress(None);
            self.last_error = None;
            self.firmware_summary = None;
            self.detected_area = None;
            self.connected_target_label = Some(target_label.clone());
            match self.preferences.connection_method {
                ConnectionMethod::Serial => {
                    self.push_log(self.text().opening_browser_port_picker.to_owned());
                }
                ConnectionMethod::Esp3d => {
                    self.push_log(self.text().trying_to_connect(&target_label));
                    if let Some(worker) = self.worker.as_ref() {
                        worker.queue_command(WorkerCommand::QueueManual(initial_probe_commands()));
                    }
                }
                ConnectionMethod::OctoPrint => {
                    self.push_log(self.text().trying_to_connect(&target_label));
                }
            }
            Ok(())
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            let target = self
                .native_target()
                .ok_or_else(|| self.text().select_serial_port_before_connecting.to_owned())?;
            let target_label = target.target_label();

            self.disconnect();

            let worker = NativeWorker::spawn(target, self.language)?;
            self.worker = Some(worker);
            self.connection_state = ConnectionState::Connecting;
            self.print_state = PrintState::Idle;
            self.set_print_progress(None);
            self.last_error = None;
            self.firmware_summary = None;
            self.detected_area = None;
            self.connected_target_label = Some(target_label.clone());
            self.push_log(self.text().trying_to_connect(&target_label));

            if self.preferences.connection_method != ConnectionMethod::OctoPrint {
                if let Some(worker) = self.worker.as_ref() {
                    if worker
                        .command_tx
                        .send(WorkerCommand::QueueManual(initial_probe_commands()))
                        .is_err()
                    {
                        self.worker = None;
                        self.connection_state = ConnectionState::Disconnected;
                        self.connected_target_label = None;
                        return Err(self.text().failed_to_start_initial_probe.to_owned());
                    }
                }
            }

            Ok(())
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn native_target(&self) -> Option<NativeConnectionTarget> {
        match self.preferences.connection_method {
            ConnectionMethod::Serial => Some(NativeConnectionTarget::Serial {
                port_name: self
                    .preferences
                    .selected_serial_port
                    .clone()
                    .or_else(|| self.available_ports.first().cloned())?,
            }),
            ConnectionMethod::Esp3d => Some(NativeConnectionTarget::Esp3d {
                endpoint: self.preferences.esp3d_endpoint.trim().to_owned(),
            }),
            ConnectionMethod::OctoPrint => Some(NativeConnectionTarget::OctoPrint {
                base_url: self.preferences.octoprint_base_url.trim().to_owned(),
                api_key: self.preferences.octoprint_api_key.trim().to_owned(),
            }),
        }
    }

    pub fn disconnect(&mut self) {
        #[cfg(target_arch = "wasm32")]
        if let Some(worker) = self.worker.take() {
            worker.queue_command(WorkerCommand::Disconnect);
            self.connection_state = ConnectionState::Disconnected;
            self.print_state = PrintState::Idle;
            self.set_print_progress(None);
            self.connected_target_label = None;
            self.push_log(self.text().closed_device_connection.to_owned());
        }

        #[cfg(not(target_arch = "wasm32"))]
        if let Some(worker) = self.worker.take() {
            let _ = worker.command_tx.send(WorkerCommand::Disconnect);
            self.push_log(self.text().closed_device_connection.to_owned());
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            self.connection_state = ConnectionState::Disconnected;
            self.print_state = PrintState::Idle;
            self.set_print_progress(None);
            self.connected_target_label = None;
        }
    }

    pub fn send_job(&mut self, gcode_lines: &[String]) -> Result<(), String> {
        if self.is_job_active() {
            return Err(self.text().print_already_in_progress.to_owned());
        }

        #[cfg(target_arch = "wasm32")]
        {
            let worker =
                self.worker.as_ref().ok_or_else(|| self.text().connect_device_first.to_owned())?;
            worker.queue_command(WorkerCommand::QueueJob(gcode_lines.to_vec()));
            self.print_state = PrintState::Printing;
            self.set_print_progress(Some(0.0));
            self.push_log(self.text().queued_gcode_lines(gcode_lines.len()));
            Ok(())
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            let worker =
                self.worker.as_ref().ok_or_else(|| self.text().connect_device_first.to_owned())?;
            worker
                .command_tx
                .send(WorkerCommand::QueueJob(gcode_lines.to_vec()))
                .map_err(|_| self.text().failed_to_queue_gcode_to_device.to_owned())?;

            self.print_state = PrintState::Printing;
            self.set_print_progress(Some(0.0));
            self.push_log(self.text().queued_gcode_lines(gcode_lines.len()));
            Ok(())
        }
    }

    pub fn stop_job(&mut self) -> Result<(), String> {
        if !self.can_stop_print() {
            return Err(self.text().no_active_print_job.to_owned());
        }

        #[cfg(target_arch = "wasm32")]
        {
            let worker =
                self.worker.as_ref().ok_or_else(|| self.text().connect_device_first.to_owned())?;
            worker.queue_command(WorkerCommand::CancelJob);
            self.print_state = PrintState::Stopping;
            self.set_print_progress(None);
            self.push_log(self.text().requested_print_stop.to_owned());
            Ok(())
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            let worker =
                self.worker.as_ref().ok_or_else(|| self.text().connect_device_first.to_owned())?;
            worker
                .command_tx
                .send(WorkerCommand::CancelJob)
                .map_err(|_| self.text().failed_to_send_stop_command.to_owned())?;

            self.print_state = PrintState::Stopping;
            self.set_print_progress(None);
            self.push_log(self.text().requested_print_stop.to_owned());
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

    pub fn motors_off(&mut self) -> Result<(), String> {
        self.queue_manual_commands(self.text().sent_motors_off, build_motors_off_commands())
    }

    pub fn move_to_first_start(
        &mut self,
        x_mm: f32,
        y_mm: f32,
        feed_rate_mm_min: f32,
    ) -> Result<(), String> {
        self.queue_manual_commands(
            self.text().sent_move_to_first_start,
            build_absolute_xy_move_commands(x_mm, y_mm, feed_rate_mm_min),
        )
    }

    pub fn move_to(&mut self, x_mm: f32, y_mm: f32, feed_rate_mm_min: f32) -> Result<(), String> {
        self.queue_manual_commands(
            self.text().sent_absolute_move,
            build_absolute_xy_move_commands(x_mm, y_mm, feed_rate_mm_min),
        )
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
            self.push_log(log_line.to_owned());
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
            self.push_log(log_line.to_owned());
            Ok(())
        }
    }

    fn push_log(&mut self, line: String) {
        if line.trim().is_empty() {
            return;
        }
        if self.log.len() == DEVICE_LOG_LIMIT {
            self.log.pop_front();
        }
        self.log.push_back(line);
    }

    pub fn tick(&mut self) -> Option<PrintableArea> {
        let mut updated_area = None;

        #[cfg(target_arch = "wasm32")]
        while let Some(event) = self.worker.as_ref().and_then(WebWorker::next_event) {
            match event {
                WorkerEvent::PortOpened => {
                    if self.connection_method() == ConnectionMethod::Serial {
                        self.push_log(self.text().opened_serial_port_waiting_firmware.to_owned());
                    }
                }
                WorkerEvent::Connected => {
                    self.connection_state = ConnectionState::Connected;
                }
                WorkerEvent::ReadyTimeout => {
                    let message = ready_timeout_message(self.language).to_owned();
                    self.last_error = Some(message.clone());
                    self.push_log(self.text().device_error(&message));
                }
                WorkerEvent::Line(line) => {
                    if let Some(firmware) = parse_firmware(&line) {
                        self.firmware_summary = Some(firmware);
                    }
                    if let Some(area) = detect_build_volume(&line) {
                        self.detected_area = Some(area);
                        updated_area = Some(area);
                    }
                    self.push_log(line);
                }
                WorkerEvent::FirmwareSummary(summary) => {
                    self.firmware_summary = Some(summary);
                }
                WorkerEvent::DetectedArea(area) => {
                    self.detected_area = Some(area);
                    updated_area = Some(area);
                }
                WorkerEvent::PrintStateChanged(state) => {
                    self.print_state = state;
                    if state != PrintState::Printing {
                        self.set_print_progress(None);
                    }
                }
                WorkerEvent::JobProgress(progress) => {
                    self.set_print_progress(Some(progress));
                }
                WorkerEvent::JobCompleted => {
                    self.print_state = PrintState::Idle;
                    self.set_print_progress(None);
                    self.push_log(self.text().printing_completed.to_owned());
                }
                WorkerEvent::JobCancelled => {
                    self.print_state = PrintState::Idle;
                    self.set_print_progress(None);
                    self.push_log(self.text().printing_stopped.to_owned());
                }
                WorkerEvent::JobFailed(error) => {
                    self.print_state = PrintState::Idle;
                    self.set_print_progress(None);
                    self.last_error = Some(error.clone());
                    self.push_log(self.text().device_error(&error));
                }
                WorkerEvent::Error(error) => {
                    self.set_print_progress(None);
                    self.last_error = Some(error.clone());
                    self.push_log(self.text().device_error(&error));
                }
                WorkerEvent::Disconnected => {
                    self.connection_state = ConnectionState::Disconnected;
                    self.print_state = PrintState::Idle;
                    self.set_print_progress(None);
                    self.connected_target_label = None;
                }
            }
        }

        #[cfg(not(target_arch = "wasm32"))]
        while let Some(event) = self.worker.as_ref().and_then(NativeWorker::next_event) {
            match event {
                WorkerEvent::PortOpened => {
                    if self.connection_method() == ConnectionMethod::Serial {
                        self.push_log(self.text().opened_serial_port_waiting_firmware.to_owned());
                    }
                }
                WorkerEvent::Connected => {
                    self.connection_state = ConnectionState::Connected;
                }
                WorkerEvent::ReadyTimeout => {
                    let message = ready_timeout_message(self.language).to_owned();
                    self.last_error = Some(message.clone());
                    self.push_log(self.text().device_error(&message));
                }
                WorkerEvent::Line(line) => {
                    if let Some(firmware) = parse_firmware(&line) {
                        self.firmware_summary = Some(firmware);
                    }
                    if let Some(area) = detect_build_volume(&line) {
                        self.detected_area = Some(area);
                        updated_area = Some(area);
                    }
                    self.push_log(line);
                }
                WorkerEvent::FirmwareSummary(summary) => {
                    self.firmware_summary = Some(summary);
                }
                WorkerEvent::DetectedArea(area) => {
                    self.detected_area = Some(area);
                    updated_area = Some(area);
                }
                WorkerEvent::PrintStateChanged(state) => {
                    self.print_state = state;
                    if state != PrintState::Printing {
                        self.set_print_progress(None);
                    }
                }
                WorkerEvent::JobProgress(progress) => {
                    self.set_print_progress(Some(progress));
                }
                WorkerEvent::JobCompleted => {
                    self.print_state = PrintState::Idle;
                    self.set_print_progress(None);
                    self.push_log(self.text().printing_completed.to_owned());
                }
                WorkerEvent::JobCancelled => {
                    self.print_state = PrintState::Idle;
                    self.set_print_progress(None);
                    self.push_log(self.text().printing_stopped.to_owned());
                }
                WorkerEvent::JobFailed(error) => {
                    self.print_state = PrintState::Idle;
                    self.set_print_progress(None);
                    self.last_error = Some(error.clone());
                    self.push_log(self.text().device_error(&error));
                }
                WorkerEvent::Error(error) => {
                    self.connection_state = ConnectionState::Disconnected;
                    self.print_state = PrintState::Idle;
                    self.set_print_progress(None);
                    self.last_error = Some(error.clone());
                    self.connected_target_label = None;
                    self.push_log(self.text().device_error(&error));
                }
                WorkerEvent::Disconnected => {
                    self.connection_state = ConnectionState::Disconnected;
                    self.print_state = PrintState::Idle;
                    self.set_print_progress(None);
                    self.connected_target_label = None;
                }
            }
        }

        updated_area
    }
}

#[cfg(not(target_arch = "wasm32"))]
enum NativeConnectionTarget {
    Serial { port_name: String },
    Esp3d { endpoint: String },
    OctoPrint { base_url: String, api_key: String },
}

#[cfg(not(target_arch = "wasm32"))]
impl NativeConnectionTarget {
    fn target_label(&self) -> String {
        match self {
            Self::Serial { port_name } => port_name.clone(),
            Self::Esp3d { endpoint } => endpoint.trim().to_owned(),
            Self::OctoPrint { base_url, .. } => base_url.trim().to_owned(),
        }
    }
}

#[cfg(target_arch = "wasm32")]
enum WebConnectionTarget {
    Serial,
    Esp3d { endpoint: String },
    OctoPrint { base_url: String, api_key: String },
}

#[cfg(target_arch = "wasm32")]
impl WebConnectionTarget {
    fn target_label(&self, language: Language) -> String {
        match self {
            Self::Serial => web_serial_device_label(language).to_owned(),
            Self::Esp3d { endpoint } => endpoint.trim().to_owned(),
            Self::OctoPrint { base_url, .. } => base_url.trim().to_owned(),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
struct NativeWorker {
    command_tx: std::sync::mpsc::Sender<WorkerCommand>,
    event_rx: std::sync::mpsc::Receiver<WorkerEvent>,
}

#[cfg(not(target_arch = "wasm32"))]
impl NativeWorker {
    fn spawn(target: NativeConnectionTarget, language: Language) -> Result<Self, String> {
        let (command_tx, command_rx) = std::sync::mpsc::channel();
        let (event_tx, event_rx) = std::sync::mpsc::channel();
        let thread_name =
            format!("penartic-device-{}", sanitize_thread_name(&target.target_label()));

        std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || run_worker(target, command_rx, event_tx, language))
            .map_err(|error| error.to_string())?;

        Ok(Self { command_tx, event_rx })
    }

    fn next_event(&self) -> Option<WorkerEvent> {
        self.event_rx.try_recv().ok()
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
    FirmwareSummary(String),
    DetectedArea(PrintableArea),
    PrintStateChanged(PrintState),
    JobProgress(f32),
    JobCompleted,
    JobCancelled,
    JobFailed(String),
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
            serial::run_serial_worker(port_name, command_rx, event_tx, language)
        }
        NativeConnectionTarget::Esp3d { endpoint } => {
            esp3d::run_esp3d_worker(endpoint, command_rx, event_tx, language)
        }
        NativeConnectionTarget::OctoPrint { base_url, api_key } => {
            octoprint::run_octoprint_worker(base_url, api_key, command_rx, event_tx, language)
        }
    }
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

pub(super) fn next_progress_update(
    progress: f32,
    last_emitted_progress_percent: &mut Option<u8>,
) -> Option<f32> {
    if !progress.is_finite() {
        return None;
    }

    let progress = progress.clamp(0.0, 1.0);
    let percent = (progress * 100.0).floor() as u8;
    if Some(percent) == *last_emitted_progress_percent {
        return None;
    }

    *last_emitted_progress_percent = Some(percent);
    Some(progress)
}

pub(super) fn queued_job_progress_update(
    total_job_lines: usize,
    queued_job_count: usize,
    in_flight_job_count: usize,
    last_emitted_progress_percent: &mut Option<u8>,
) -> Option<f32> {
    if total_job_lines == 0 {
        return None;
    }

    let outstanding = queued_job_count.saturating_add(in_flight_job_count).min(total_job_lines);
    let completed = total_job_lines.saturating_sub(outstanding);
    if completed >= total_job_lines {
        *last_emitted_progress_percent = Some(100);
        return None;
    }

    next_progress_update(completed as f32 / total_job_lines as f32, last_emitted_progress_percent)
}

fn initial_probe_commands() -> Vec<String> {
    vec!["M115".to_owned(), "M503".to_owned(), "M211".to_owned()]
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

fn build_absolute_xy_move_commands(x_mm: f32, y_mm: f32, feed_rate_mm_min: f32) -> Vec<String> {
    vec![
        "G21".to_owned(),
        "M400".to_owned(),
        "G90".to_owned(),
        build_absolute_xy_move_command(x_mm, y_mm, feed_rate_mm_min),
        "M400".to_owned(),
    ]
}

fn build_motors_off_commands() -> Vec<String> {
    vec!["M400".to_owned(), "M84".to_owned()]
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

    fn pop(&self) -> Option<WorkerEvent> {
        self.0.borrow_mut().pop_front()
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
impl WebWorker {
    fn spawn(target: WebConnectionTarget, language: Language) -> Self {
        let commands = WebCommandQueue::default();
        let events = WebEventQueue::default();
        match target {
            WebConnectionTarget::Serial => {
                spawn_local(serial::run_web_serial_worker(
                    commands.clone(),
                    events.clone(),
                    language,
                ));
            }
            WebConnectionTarget::Esp3d { endpoint } => {
                spawn_local(esp3d::run_websocket_worker(
                    commands.clone(),
                    events.clone(),
                    endpoint,
                    language,
                ));
            }
            WebConnectionTarget::OctoPrint { base_url, api_key } => {
                spawn_local(octoprint::run_web_worker(
                    commands.clone(),
                    events.clone(),
                    base_url,
                    api_key,
                    language,
                ));
            }
        }
        Self { commands, events }
    }

    fn queue_command(&self, command: WorkerCommand) {
        self.commands.push(command);
    }

    fn next_event(&self) -> Option<WorkerEvent> {
        self.events.pop()
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
    fn connection_methods_include_octoprint() {
        assert!(ConnectionMethod::available().contains(&ConnectionMethod::OctoPrint));
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
    fn throttles_progress_updates_by_whole_percent() {
        let mut last_percent = None;
        let first = next_progress_update(0.101, &mut last_percent).unwrap();
        assert!((first - 0.101).abs() < f32::EPSILON);
        assert_eq!(next_progress_update(0.109, &mut last_percent), None);

        let second = next_progress_update(0.111, &mut last_percent).unwrap();
        assert!((second - 0.111).abs() < f32::EPSILON);
    }

    #[test]
    fn queue_progress_stops_emitting_once_job_completes() {
        let mut last_percent = None;
        let progress = queued_job_progress_update(4, 1, 2, &mut last_percent).unwrap();
        assert!((progress - 0.25).abs() < f32::EPSILON);
        assert_eq!(queued_job_progress_update(4, 1, 2, &mut last_percent), None);
        assert_eq!(queued_job_progress_update(4, 0, 0, &mut last_percent), None);
        assert_eq!(last_percent, Some(100));
    }

    #[test]
    fn builds_absolute_xy_move_sequence_for_manual_positioning() {
        let commands = build_absolute_xy_move_commands(12.5, 4.0, 1800.0);
        assert_eq!(
            commands,
            vec![
                "G21".to_owned(),
                "M400".to_owned(),
                "G90".to_owned(),
                "G1 X12.50 Y4.00 F1800".to_owned(),
                "M400".to_owned(),
            ]
        );
    }

    #[test]
    fn builds_motors_off_sequence() {
        assert_eq!(build_motors_off_commands(), vec!["M400".to_owned(), "M84".to_owned()]);
    }

    #[test]
    fn normalizes_http_esp3d_address_to_data_websocket_port() {
        assert_eq!(
            super::esp3d::normalize_esp3d_endpoint("http://192.168.0.112/").unwrap(),
            "ws://192.168.0.112:8282/"
        );
        assert_eq!(
            super::esp3d::normalize_esp3d_endpoint("http://192.168.0.112:80/").unwrap(),
            "ws://192.168.0.112:8282/"
        );
        assert_eq!(
            super::esp3d::normalize_esp3d_endpoint("ws://192.168.0.112:8282/").unwrap(),
            "ws://192.168.0.112:8282/"
        );
        assert_eq!(
            super::esp3d::normalize_esp3d_endpoint("https://esp3d.local/").unwrap(),
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
        let mut total_job_lines = 1;
        let mut last_progress_percent = None;
        let queued_job_count = 0;
        let mut job_active = true;
        let mut job_cancelled = false;
        let (event_tx, event_rx) = std::sync::mpsc::channel();

        super::esp3d::handle_esp3d_websocket_text(
            "ok\nok\n",
            &mut in_flight,
            &mut in_flight_job_count,
            &mut total_job_lines,
            &queued_job_count,
            &mut job_active,
            &mut job_cancelled,
            &mut last_progress_percent,
            &event_tx,
            Language::English,
        )
        .unwrap();

        assert_eq!(in_flight_job_count, 0);
        assert_eq!(total_job_lines, 0);
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
        let mut total_job_lines = 2;
        let mut last_progress_percent = None;
        let queued_job_count = 0;
        let mut job_active = true;
        let mut job_cancelled = false;
        let (event_tx, event_rx) = std::sync::mpsc::channel();

        super::esp3d::handle_esp3d_websocket_text(
            "okok",
            &mut in_flight,
            &mut in_flight_job_count,
            &mut total_job_lines,
            &queued_job_count,
            &mut job_active,
            &mut job_cancelled,
            &mut last_progress_percent,
            &event_tx,
            Language::English,
        )
        .unwrap();

        assert_eq!(in_flight_job_count, 0);
        assert_eq!(total_job_lines, 0);
        assert!(!job_active);
        assert!(
            matches!(event_rx.try_recv(), Ok(WorkerEvent::JobProgress(progress)) if (progress - 0.5).abs() < f32::EPSILON)
        );
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
