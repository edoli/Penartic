use std::collections::VecDeque;

use crate::plot::model::PrintableArea;

const DEVICE_LOG_LIMIT: usize = 48;

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Unsupported,
    Disconnected,
    Connecting,
    Connected,
}

pub struct DeviceController {
    available_ports: Vec<String>,
    selected_port: Option<String>,
    connection_state: ConnectionState,
    firmware_summary: Option<String>,
    detected_area: Option<PrintableArea>,
    log: VecDeque<String>,
    last_error: Option<String>,
    #[cfg(not(target_arch = "wasm32"))]
    worker: Option<NativeWorker>,
}

impl DeviceController {
    pub fn new() -> Self {
        #[allow(unused_mut)]
        let mut controller = Self {
            available_ports: Vec::new(),
            selected_port: None,
            connection_state: ConnectionState::Disconnected,
            firmware_summary: None,
            detected_area: None,
            log: VecDeque::new(),
            last_error: None,
            #[cfg(not(target_arch = "wasm32"))]
            worker: None,
        };

        #[cfg(target_arch = "wasm32")]
        {
            controller.connection_state = ConnectionState::Unsupported;
            controller.push_log("웹 빌드에서는 오프라인 미리보기와 G-code 복사만 지원합니다.");
        }

        controller
    }

    pub fn refresh_ports(&mut self) {
        #[cfg(target_arch = "wasm32")]
        {
            self.available_ports.clear();
            self.connection_state = ConnectionState::Unsupported;
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
            ConnectionState::Disconnected => "Disconnected".to_owned(),
            ConnectionState::Connecting => "Connecting…".to_owned(),
            ConnectionState::Connected => match &self.selected_port {
                Some(port) => format!("Connected: {port}"),
                None => "Connected".to_owned(),
            },
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

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn log_lines(&self) -> impl DoubleEndedIterator<Item = &str> {
        self.log.iter().map(String::as_str)
    }

    pub fn connect(&mut self) -> Result<(), String> {
        #[cfg(target_arch = "wasm32")]
        {
            self.connection_state = ConnectionState::Unsupported;
            return Err("웹 빌드에서는 실제 장치 연결을 지원하지 않습니다.".to_owned());
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
            self.last_error = None;
            self.firmware_summary = None;
            self.detected_area = None;
            self.selected_port = Some(port_name.clone());
            self.push_log(format!("{port_name} 에 연결을 시도합니다."));

            if command_tx
                .send(WorkerCommand::Queue(vec!["M115".to_owned(), "M503".to_owned()]))
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
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(worker) = self.worker.take() {
            let _ = worker.command_tx.send(WorkerCommand::Disconnect);
            self.push_log("장치 연결을 종료했습니다.");
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            self.connection_state = ConnectionState::Disconnected;
        }
    }

    pub fn send_job(&mut self, gcode_lines: &[String]) -> Result<(), String> {
        #[cfg(target_arch = "wasm32")]
        {
            let _ = gcode_lines;
            return Err("웹 빌드에서는 장치로 직접 출력할 수 없습니다.".to_owned());
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            let worker =
                self.worker.as_ref().ok_or_else(|| "먼저 장치를 연결하세요.".to_owned())?;

            worker
                .command_tx
                .send(WorkerCommand::Queue(gcode_lines.to_vec()))
                .map_err(|_| "장치 작업 큐에 G-code를 전달하지 못했습니다.".to_owned())?;

            self.push_log(format!("G-code {}줄을 장치로 전송 큐에 올렸습니다.", gcode_lines.len()));
            Ok(())
        }
    }

    pub fn tick(&mut self) -> Option<PrintableArea> {
        #[cfg(target_arch = "wasm32")]
        {
            return None;
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
                        self.push_log(format!("장치 오류: {message}"));
                        self.worker = None;
                        break;
                    }
                    WorkerEvent::Disconnected => {
                        self.connection_state = ConnectionState::Disconnected;
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

#[cfg(not(target_arch = "wasm32"))]
enum WorkerCommand {
    Queue(Vec<String>),
    Disconnect,
}

#[cfg(not(target_arch = "wasm32"))]
enum WorkerEvent {
    Connected,
    Line(String),
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
    use std::time::{Duration, Instant};

    let result = (|| -> Result<(), String> {
        let mut port = serialport::new(&port_name, 115_200)
            .timeout(Duration::from_millis(100))
            .open()
            .map_err(|error| error.to_string())?;

        event_tx.send(WorkerEvent::Connected).map_err(|error| error.to_string())?;

        let mut queued_lines = VecDeque::new();
        let mut waiting_for_ok_since: Option<Instant> = None;
        let mut read_buffer = [0_u8; 512];
        let mut pending_text = String::new();

        loop {
            while let Ok(command) = command_rx.try_recv() {
                match command {
                    WorkerCommand::Queue(lines) => queued_lines.extend(lines),
                    WorkerCommand::Disconnect => return Ok(()),
                }
            }

            if waiting_for_ok_since.is_none() {
                if let Some(line) = queued_lines.pop_front() {
                    write_line(&mut *port, &line).map_err(|error| error.to_string())?;
                    waiting_for_ok_since = Some(Instant::now());
                }
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

                        if waiting_for_ok_since.is_some() && is_ack_line(&line) {
                            waiting_for_ok_since = None;
                        }

                        event_tx
                            .send(WorkerEvent::Line(line))
                            .map_err(|error| error.to_string())?;
                    }
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
                    if waiting_for_ok_since
                        .is_some_and(|started| started.elapsed() > Duration::from_secs(2))
                    {
                        waiting_for_ok_since = None;
                    }

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
fn write_line(port: &mut dyn serialport::SerialPort, line: &str) -> std::io::Result<()> {
    port.write_all(line.as_bytes())?;
    port.write_all(b"\n")?;
    port.flush()
}

#[cfg(not(target_arch = "wasm32"))]
fn parse_firmware(line: &str) -> Option<String> {
    let upper = line.to_ascii_uppercase();
    if upper.contains("FIRMWARE_NAME") || upper.contains("MACHINE_TYPE") {
        Some(line.to_owned())
    } else {
        None
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn is_ack_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower == "ok" || lower.starts_with("ok ")
}

#[cfg(not(target_arch = "wasm32"))]
fn detect_build_volume(line: &str) -> Option<PrintableArea> {
    let upper = line.to_ascii_uppercase();
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

#[cfg(not(target_arch = "wasm32"))]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_m208_size_report() {
        let size = detect_build_volume("echo:  M208 X220.00 Y220.00 Z250.00 S0").unwrap();
        assert_eq!(size.width_mm, 220.0);
        assert_eq!(size.height_mm, 220.0);
    }
}
