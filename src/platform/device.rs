use std::{collections::VecDeque, time::Duration};

use crate::plot::model::PrintableArea;

#[cfg(target_arch = "wasm32")]
use {
    js_sys::{Function, Object, Reflect, Uint8Array},
    std::{cell::RefCell, rc::Rc},
    wasm_bindgen::{JsCast as _, JsValue},
    wasm_bindgen_futures::{JsFuture, spawn_local},
};

const DEVICE_LOG_LIMIT: usize = 48;
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
const MAX_IN_FLIGHT_LINES: usize = 8;
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
    available_ports: Vec<String>,
    selected_port: Option<String>,
    connection_state: ConnectionState,
    print_state: PrintState,
    firmware_summary: Option<String>,
    detected_area: Option<PrintableArea>,
    log: VecDeque<String>,
    last_error: Option<String>,
    #[cfg(not(target_arch = "wasm32"))]
    worker: Option<NativeWorker>,
    #[cfg(target_arch = "wasm32")]
    worker: Option<WebWorker>,
}

impl DeviceController {
    pub fn new() -> Self {
        #[allow(unused_mut)]
        let mut controller = Self {
            available_ports: Vec::new(),
            selected_port: None,
            connection_state: ConnectionState::Disconnected,
            print_state: PrintState::Idle,
            firmware_summary: None,
            detected_area: None,
            log: VecDeque::new(),
            last_error: None,
            #[cfg(not(target_arch = "wasm32"))]
            worker: None,
            #[cfg(target_arch = "wasm32")]
            worker: None,
        };

        #[cfg(target_arch = "wasm32")]
        {
            if web_serial_api().is_some() {
                controller.selected_port = Some("브라우저에서 포트 선택".to_owned());
                controller.available_ports.push("브라우저에서 포트 선택".to_owned());
                controller.push_log("Web Serial API로 장치 연결을 사용할 수 있습니다.");
            } else {
                controller.connection_state = ConnectionState::Unsupported;
                controller.push_log(
                    "이 브라우저는 Web Serial API를 지원하지 않습니다. Chrome/Edge의 HTTPS 또는 localhost에서 실행하세요.",
                );
            }
        }

        controller
    }

    pub fn refresh_ports(&mut self) {
        #[cfg(target_arch = "wasm32")]
        {
            self.available_ports.clear();
            if web_serial_api().is_none() {
                self.connection_state = ConnectionState::Unsupported;
                self.selected_port = None;
                return;
            }

            self.selected_port = Some("브라우저에서 포트 선택".to_owned());
            self.available_ports.push("브라우저에서 포트 선택".to_owned());
            self.push_log("연결 버튼을 누르면 브라우저에서 Web Serial 포트를 선택합니다.");
            return;
        }

        #[cfg(not(target_arch = "wasm32"))]
        match serialport::available_ports() {
            Ok(ports) => {
                self.available_ports = ports.into_iter().map(|port| port.port_name).collect();
                if self.selected_port.is_none() {
                    self.selected_port = self.available_ports.first().cloned();
                }
                self.push_log(format!(
                    "시리얼 포트 {}개를 찾았습니다.",
                    self.available_ports.len()
                ));
                self.last_error = None;
            }
            Err(error) => {
                self.available_ports.clear();
                self.last_error = Some(error.to_string());
                self.push_log(format!("포트 목록을 읽지 못했습니다: {error}"));
            }
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

    pub fn connection_state(&self) -> ConnectionState {
        self.connection_state
    }

    pub fn status_text(&self) -> String {
        match self.connection_state {
            ConnectionState::Unsupported => "Web preview only".to_owned(),
            ConnectionState::Disconnected => "연결 안됨".to_owned(),
            ConnectionState::Connecting => "연결 중…".to_owned(),
            ConnectionState::Connected => match &self.selected_port {
                Some(port) => format!("연결됨: {port}"),
                None => "연결됨".to_owned(),
            },
        }
    }

    pub fn print_state(&self) -> PrintState {
        self.print_state
    }

    pub fn print_state_text(&self) -> &'static str {
        match self.print_state {
            PrintState::Idle => "대기 중",
            PrintState::Printing => "프린트 중",
            PrintState::Stopping => "정지 요청됨",
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
            self.connection_state == ConnectionState::Disconnected && web_serial_api().is_some()
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            !self.available_ports.is_empty()
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
            if web_serial_api().is_none() {
                self.connection_state = ConnectionState::Unsupported;
                return Err(
                    "이 브라우저는 Web Serial API를 지원하지 않습니다. Chrome/Edge의 HTTPS 또는 localhost에서 실행하세요."
                        .to_owned(),
                );
            }

            self.disconnect();
            let worker = WebWorker::spawn();
            self.worker = Some(worker);
            self.connection_state = ConnectionState::Connecting;
            self.print_state = PrintState::Idle;
            self.last_error = None;
            self.firmware_summary = None;
            self.detected_area = None;
            self.selected_port = Some("Web Serial 장치".to_owned());
            self.push_log("브라우저 포트 선택 창을 엽니다.");
            Ok(())
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            let port_name = self
                .selected_port
                .clone()
                .or_else(|| self.available_ports.first().cloned())
                .ok_or_else(|| "먼저 연결할 시리얼 포트를 선택하세요.".to_owned())?;

            self.disconnect();

            let (worker, command_tx) = NativeWorker::spawn(port_name.clone())?;
            self.worker = Some(worker);
            self.connection_state = ConnectionState::Connecting;
            self.print_state = PrintState::Idle;
            self.last_error = None;
            self.firmware_summary = None;
            self.detected_area = None;
            self.selected_port = Some(port_name.clone());
            self.push_log(format!("{port_name} 에 연결을 시도합니다."));

            if command_tx
                .send(WorkerCommand::QueueManual(vec![
                    "M115".to_owned(),
                    "M503".to_owned(),
                    "M211".to_owned(),
                ]))
                .is_err()
            {
                self.worker = None;
                self.connection_state = ConnectionState::Disconnected;
                return Err("장치 초기 프로브를 시작하지 못했습니다.".to_owned());
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
            self.push_log("장치 연결을 종료했습니다.");
        }

        #[cfg(not(target_arch = "wasm32"))]
        if let Some(worker) = self.worker.take() {
            let _ = worker.command_tx.send(WorkerCommand::Disconnect);
            self.push_log("장치 연결을 종료했습니다.");
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
                return Err("이미 프린트가 진행 중입니다.".to_owned());
            }

            let worker =
                self.worker.as_ref().ok_or_else(|| "먼저 장치를 연결하세요.".to_owned())?;
            worker.queue_command(WorkerCommand::QueueJob(gcode_lines.to_vec()));
            self.print_state = PrintState::Printing;
            self.push_log(format!("G-code {}줄을 장치로 전송 큐에 올렸습니다.", gcode_lines.len()));
            Ok(())
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.is_job_active() {
                return Err("이미 프린트가 진행 중입니다.".to_owned());
            }

            let worker =
                self.worker.as_ref().ok_or_else(|| "먼저 장치를 연결하세요.".to_owned())?;

            worker
                .command_tx
                .send(WorkerCommand::QueueJob(gcode_lines.to_vec()))
                .map_err(|_| "장치 작업 큐에 G-code를 전달하지 못했습니다.".to_owned())?;

            self.print_state = PrintState::Printing;
            self.push_log(format!("G-code {}줄을 장치로 전송 큐에 올렸습니다.", gcode_lines.len()));
            Ok(())
        }
    }

    pub fn stop_job(&mut self) -> Result<(), String> {
        #[cfg(target_arch = "wasm32")]
        {
            if !self.can_stop_print() {
                return Err("현재 중지할 프린트 작업이 없습니다.".to_owned());
            }

            let worker =
                self.worker.as_ref().ok_or_else(|| "먼저 장치를 연결하세요.".to_owned())?;
            worker.queue_command(WorkerCommand::CancelJob);
            self.print_state = PrintState::Stopping;
            self.push_log("프린트 중지를 요청했습니다. 장치가 지원하면 즉시 정지합니다.");
            Ok(())
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            if !self.can_stop_print() {
                return Err("현재 중지할 프린트 작업이 없습니다.".to_owned());
            }

            let worker =
                self.worker.as_ref().ok_or_else(|| "먼저 장치를 연결하세요.".to_owned())?;
            worker
                .command_tx
                .send(WorkerCommand::CancelJob)
                .map_err(|_| "프린트 중지 명령을 전달하지 못했습니다.".to_owned())?;

            self.print_state = PrintState::Stopping;
            self.push_log("프린트 중지를 요청했습니다. 장치가 지원하면 즉시 정지합니다.");
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
            .ok_or_else(|| "이동할 축이 없습니다.".to_owned())?;
        self.queue_manual_commands(
            "수동 X/Y 이동 명령을 전송했습니다.",
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
            .ok_or_else(|| "이동할 축이 없습니다.".to_owned())?;
        self.queue_manual_commands(
            "수동 Z 이동 명령을 전송했습니다.",
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
            "XY 홈 이동 명령을 전송했습니다.",
            vec!["G21".to_owned(), "M400".to_owned(), "G28 X Y".to_owned()],
        )
    }

    pub fn home_z(&mut self) -> Result<(), String> {
        self.queue_manual_commands(
            "Z 홈 이동 명령을 전송했습니다.",
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
            .ok_or_else(|| "Z 리프트 이동 거리가 없습니다.".to_owned())?;
        self.queue_manual_commands(
            "첫 그리기 시작 위치 이동 명령을 전송했습니다.",
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
                        self.push_log("시리얼 포트를 열었습니다. 펌웨어 응답을 기다립니다.");
                    }
                    WorkerEvent::Connected => {
                        self.connection_state = ConnectionState::Connected;
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
                        self.push_log(format!("장치 오류: {message}"));
                        self.worker = None;
                        break;
                    }
                    WorkerEvent::JobCompleted => {
                        self.print_state = PrintState::Idle;
                        self.push_log("프린트가 완료되었습니다.");
                    }
                    WorkerEvent::JobCancelled => {
                        self.print_state = PrintState::Idle;
                        self.push_log("프린트가 중지되었습니다.");
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
                        self.push_log("시리얼 포트를 열었습니다. 펌웨어 응답을 기다립니다.");
                    }
                    WorkerEvent::Connected => {
                        self.connection_state = ConnectionState::Connected;
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
                        self.push_log(format!("장치 오류: {message}"));
                        self.worker = None;
                        break;
                    }
                    WorkerEvent::JobCompleted => {
                        self.print_state = PrintState::Idle;
                        self.push_log("프린트가 완료되었습니다.");
                    }
                    WorkerEvent::JobCancelled => {
                        self.print_state = PrintState::Idle;
                        self.push_log("프린트가 중지되었습니다.");
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
                return Err("프린트 중에는 수동 제어를 사용할 수 없습니다.".to_owned());
            }

            let worker =
                self.worker.as_ref().ok_or_else(|| "먼저 장치를 연결하세요.".to_owned())?;
            worker.queue_command(WorkerCommand::QueueManual(commands));
            self.push_log(log_line);
            Ok(())
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.is_job_active() {
                return Err("프린트 중에는 수동 제어를 사용할 수 없습니다.".to_owned());
            }

            let worker =
                self.worker.as_ref().ok_or_else(|| "먼저 장치를 연결하세요.".to_owned())?;
            worker
                .command_tx
                .send(WorkerCommand::QueueManual(commands))
                .map_err(|_| "수동 제어 명령을 전달하지 못했습니다.".to_owned())?;
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
impl NativeWorker {
    fn spawn(port_name: String) -> Result<(Self, std::sync::mpsc::Sender<WorkerCommand>), String> {
        let (command_tx, command_rx) = std::sync::mpsc::channel();
        let (event_tx, event_rx) = std::sync::mpsc::channel();
        let thread_command_tx = command_tx.clone();

        std::thread::Builder::new()
            .name(format!("penartic-serial-{port_name}"))
            .spawn(move || run_worker(port_name, command_rx, event_tx))
            .map_err(|error| error.to_string())?;

        Ok((Self { command_tx: command_tx.clone(), event_rx }, thread_command_tx))
    }
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
    Line(String),
    JobCompleted,
    JobCancelled,
    Error(String),
    Disconnected,
}

#[cfg(not(target_arch = "wasm32"))]
fn run_worker(
    port_name: String,
    command_rx: std::sync::mpsc::Receiver<WorkerCommand>,
    event_tx: std::sync::mpsc::Sender<WorkerEvent>,
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
        let mut in_flight_sources = VecDeque::new();
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
                    return Err("펌웨어가 준비 응답을 보내지 않았습니다.".to_owned());
                }
            }

            while ready && in_flight_sources.len() < MAX_IN_FLIGHT_LINES {
                let mut batch = Vec::new();

                while in_flight_sources.len() < MAX_IN_FLIGHT_LINES {
                    let Some(queued) = queued_lines.pop_front() else {
                        break;
                    };

                    batch.extend_from_slice(queued.line.as_bytes());
                    batch.push(b'\n');

                    if queued.source == QueuedLineSource::Job {
                        queued_job_count = queued_job_count.saturating_sub(1);
                        in_flight_job_count += 1;
                    }
                    in_flight_sources.push_back(queued.source);

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
                            if let Some(source) = in_flight_sources.pop_front() {
                                if source == QueuedLineSource::Job {
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

fn is_ready_line(line: &str) -> bool {
    let upper = line.to_ascii_uppercase();
    is_ack_line(line) || upper.contains("FIRMWARE_NAME") || upper == "START"
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
impl WebWorker {
    fn spawn() -> Self {
        let commands = WebCommandQueue::default();
        let events = WebEventQueue::default();
        spawn_local(run_web_worker(commands.clone(), events.clone()));
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
async fn run_web_worker(commands: WebCommandQueue, events: WebEventQueue) {
    let result = run_web_worker_inner(commands, events.clone()).await;
    if let Err(error) = result {
        events.push(WorkerEvent::Error(error));
    }
    events.push(WorkerEvent::Disconnected);
}

#[cfg(target_arch = "wasm32")]
async fn run_web_worker_inner(
    commands: WebCommandQueue,
    events: WebEventQueue,
) -> Result<(), String> {
    let serial = web_serial_api().ok_or_else(|| {
        "이 브라우저는 Web Serial API를 지원하지 않습니다. Chrome/Edge의 HTTPS 또는 localhost에서 실행하세요."
            .to_owned()
    })?;
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

    let shared = WebSerialShared::new(commands, events.clone(), writer, reader.clone());
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
    writer: JsValue,
    reader: JsValue,
    queued_lines: VecDeque<QueuedLine>,
    queued_job_count: usize,
    in_flight_sources: VecDeque<QueuedLineSource>,
    in_flight_job_count: usize,
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
    ) -> Self {
        Self(Rc::new(RefCell::new(WebSerialState {
            commands,
            events,
            writer,
            reader,
            queued_lines: VecDeque::new(),
            queued_job_count: 0,
            in_flight_sources: VecDeque::new(),
            in_flight_job_count: 0,
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
            state.queued_lines.extend(["M115", "M503", "M211"].into_iter().map(|line| {
                QueuedLine { line: line.to_owned(), source: QueuedLineSource::Manual }
            }));
            state.events.push(WorkerEvent::Connected);
        }

        if state.ready && is_ack_line(line) {
            if let Some(source) = state.in_flight_sources.pop_front() {
                if source == QueuedLineSource::Job {
                    state.in_flight_job_count = state.in_flight_job_count.saturating_sub(1);
                    if state.queued_job_count == 0 && state.in_flight_job_count == 0 {
                        state.events.push(if state.job_cancelled {
                            WorkerEvent::JobCancelled
                        } else {
                            WorkerEvent::JobCompleted
                        });
                        state.job_cancelled = false;
                    }
                }
            }
        }

        state.events.push(WorkerEvent::Line(line.to_owned()));
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
                    state.events.push(WorkerEvent::Error(
                        "펌웨어가 준비 응답을 보내지 않았습니다.".to_owned(),
                    ));
                    state.disconnect_requested = true;
                    None
                } else {
                    None
                }
            } else if state.in_flight_sources.len() < MAX_IN_FLIGHT_LINES {
                let mut batch = Vec::new();
                while state.in_flight_sources.len() < MAX_IN_FLIGHT_LINES {
                    let Some(queued) = state.queued_lines.pop_front() else {
                        break;
                    };

                    batch.extend_from_slice(queued.line.as_bytes());
                    batch.push(b'\n');
                    if queued.source == QueuedLineSource::Job {
                        state.queued_job_count = state.queued_job_count.saturating_sub(1);
                        state.in_flight_job_count += 1;
                    }
                    state.in_flight_sources.push_back(queued.source);

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
}
