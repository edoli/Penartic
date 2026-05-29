use super::*;

use std::time::Duration;

use reqwest::{
    Method, StatusCode, Url,
    blocking::{Client, multipart},
};
use serde::{Deserialize, Serialize};

const OCTOPRINT_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const OCTOPRINT_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const OCTOPRINT_POLL_INTERVAL: Duration = Duration::from_millis(500);
const OCTOPRINT_JOB_FILE_NAME: &str = "penartic-job.gcode";

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn run_octoprint_worker(
    base_url: String,
    api_key: String,
    command_rx: std::sync::mpsc::Receiver<WorkerCommand>,
    event_tx: std::sync::mpsc::Sender<WorkerEvent>,
    language: Language,
) {
    let result = (|| -> Result<(), String> {
        let service = OctoPrintService::new(&base_url, &api_key, language)?;
        let version = service.version()?;
        let profiles = service.printer_profiles()?;
        let printer = service.printer()?;
        ensure_printer_ready(&printer, language)?;

        if let Some(summary) = version.summary() {
            event_tx
                .send(WorkerEvent::FirmwareSummary(summary))
                .map_err(|error| error.to_string())?;
        }

        if let Some(area) = printable_area_from_profiles(&profiles) {
            event_tx.send(WorkerEvent::DetectedArea(area)).map_err(|error| error.to_string())?;
        }

        event_tx.send(WorkerEvent::Connected).map_err(|error| error.to_string())?;
        event_tx
            .send(WorkerEvent::Line(language.strings().octoprint_http_connected.to_owned()))
            .map_err(|error| error.to_string())?;

        let mut observed_print_state = print_state_from_printer(&printer);
        if observed_print_state != PrintState::Idle {
            event_tx
                .send(WorkerEvent::PrintStateChanged(observed_print_state))
                .map_err(|error| error.to_string())?;
        }

        loop {
            match command_rx.recv_timeout(OCTOPRINT_POLL_INTERVAL) {
                Ok(WorkerCommand::Disconnect) => return Ok(()),
                Ok(command) => {
                    if handle_worker_command(
                        &service,
                        command,
                        &command_rx,
                        &event_tx,
                        &mut observed_print_state,
                    )? {
                        return Ok(());
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            }

            let printer = service.printer()?;
            ensure_printer_ready(&printer, language)?;
            let next_print_state = print_state_from_printer(&printer);
            emit_print_state_transition(observed_print_state, next_print_state, &event_tx)?;
            observed_print_state = next_print_state;
        }
    })();

    if let Err(error) = result {
        let _ = event_tx.send(WorkerEvent::Error(error));
    }

    let _ = event_tx.send(WorkerEvent::Disconnected);
}

#[cfg(not(target_arch = "wasm32"))]
fn handle_worker_command(
    service: &OctoPrintService,
    first_command: WorkerCommand,
    command_rx: &std::sync::mpsc::Receiver<WorkerCommand>,
    event_tx: &std::sync::mpsc::Sender<WorkerEvent>,
    observed_print_state: &mut PrintState,
) -> Result<bool, String> {
    if matches!(first_command, WorkerCommand::Disconnect) {
        return Ok(true);
    }
    process_worker_command(service, first_command, event_tx, observed_print_state)?;
    while let Ok(command) = command_rx.try_recv() {
        if matches!(command, WorkerCommand::Disconnect) {
            return Ok(true);
        }
        process_worker_command(service, command, event_tx, observed_print_state)?;
    }
    Ok(false)
}

#[cfg(not(target_arch = "wasm32"))]
fn process_worker_command(
    service: &OctoPrintService,
    command: WorkerCommand,
    event_tx: &std::sync::mpsc::Sender<WorkerEvent>,
    observed_print_state: &mut PrintState,
) -> Result<(), String> {
    match command {
        WorkerCommand::QueueJob(lines) => {
            let lines = clean_gcode_lines(lines);
            if lines.is_empty() {
                return Ok(());
            }

            if let Err(error) =
                service.upload_job(&lines).and_then(|()| service.start_uploaded_job())
            {
                event_tx
                    .send(WorkerEvent::JobFailed(error))
                    .map_err(|send_error| send_error.to_string())?;
                return Ok(());
            }

            if *observed_print_state != PrintState::Printing {
                *observed_print_state = PrintState::Printing;
                event_tx
                    .send(WorkerEvent::PrintStateChanged(PrintState::Printing))
                    .map_err(|error| error.to_string())?;
            }
        }
        WorkerCommand::QueueManual(lines) => {
            let lines = clean_gcode_lines(lines);
            if !lines.is_empty() {
                service.send_commands(&lines)?;
            }
        }
        WorkerCommand::CancelJob => {
            service.cancel_job()?;
            if *observed_print_state != PrintState::Stopping {
                *observed_print_state = PrintState::Stopping;
                event_tx
                    .send(WorkerEvent::PrintStateChanged(PrintState::Stopping))
                    .map_err(|error| error.to_string())?;
            }
        }
        WorkerCommand::Disconnect => {}
    }

    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn emit_print_state_transition(
    previous: PrintState,
    next: PrintState,
    event_tx: &std::sync::mpsc::Sender<WorkerEvent>,
) -> Result<(), String> {
    if previous == next {
        return Ok(());
    }

    match (previous, next) {
        (PrintState::Printing, PrintState::Idle) => {
            event_tx.send(WorkerEvent::JobCompleted).map_err(|error| error.to_string())?;
        }
        (PrintState::Stopping, PrintState::Idle) => {
            event_tx.send(WorkerEvent::JobCancelled).map_err(|error| error.to_string())?;
        }
        (_, next_state) => {
            event_tx
                .send(WorkerEvent::PrintStateChanged(next_state))
                .map_err(|error| error.to_string())?;
        }
    }

    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn ensure_printer_ready(
    printer: &OctoPrintPrinterResponse,
    language: Language,
) -> Result<(), String> {
    if printer.state.flags.is_ready_for_commands() {
        Ok(())
    } else {
        Err(language.strings().octoprint_printer_not_ready.to_owned())
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn print_state_from_printer(printer: &OctoPrintPrinterResponse) -> PrintState {
    if printer.state.flags.cancelling {
        PrintState::Stopping
    } else if printer.state.flags.printing
        || printer.state.flags.paused
        || printer.state.flags.pausing
        || printer.state.flags.resuming
        || printer.state.flags.finishing
    {
        PrintState::Printing
    } else {
        PrintState::Idle
    }
}

fn printable_area_from_profiles(
    response: &OctoPrintPrinterProfilesResponse,
) -> Option<PrintableArea> {
    let current_profile = response
        .profiles
        .values()
        .find(|profile| profile.current)
        .or_else(|| response.profiles.values().find(|profile| profile.default))
        .or_else(|| response.profiles.values().next())?;

    let volume = current_profile.volume.as_ref()?;
    let width_mm = volume.width?;
    let depth_mm = volume.depth?;
    let scale = if volume.units.as_deref() == Some("in") { 25.4 } else { 1.0 };

    Some(PrintableArea::new(width_mm * scale, depth_mm * scale))
}

fn normalize_octoprint_base_url(base_url: &str, language: Language) -> Result<String, String> {
    let trimmed = base_url.trim();
    if trimmed.is_empty() {
        return Err(language.strings().octoprint_address_required.to_owned());
    }

    let candidate =
        if trimmed.contains("://") { trimmed.to_owned() } else { format!("http://{trimmed}") };

    let normalized = Url::parse(&candidate).map_err(|error| {
        format!("{}: {error}", language.strings().octoprint_http_request_failed)
    })?;

    Ok(normalized.to_string().trim_end_matches('/').to_owned())
}

#[cfg(not(target_arch = "wasm32"))]
struct OctoPrintService {
    client: Client,
    base_url: String,
    api_key: String,
    language: Language,
}

#[cfg(not(target_arch = "wasm32"))]
impl OctoPrintService {
    fn new(base_url: &str, api_key: &str, language: Language) -> Result<Self, String> {
        let client = Client::builder()
            .connect_timeout(OCTOPRINT_CONNECT_TIMEOUT)
            .timeout(OCTOPRINT_REQUEST_TIMEOUT)
            .build()
            .map_err(|error| {
                format!("{}: {error}", language.strings().octoprint_http_request_failed)
            })?;

        Ok(Self {
            client,
            base_url: normalize_octoprint_base_url(base_url, language)?,
            api_key: api_key.trim().to_owned(),
            language,
        })
    }

    fn version(&self) -> Result<OctoPrintVersionResponse, String> {
        self.get_json("/api/version")
    }

    fn printer_profiles(&self) -> Result<OctoPrintPrinterProfilesResponse, String> {
        self.get_json("/api/printerprofiles")
    }

    fn printer(&self) -> Result<OctoPrintPrinterResponse, String> {
        self.get_json("/api/printer")
    }

    fn send_commands(&self, commands: &[String]) -> Result<(), String> {
        self.post_json("/api/printer/command", &OctoPrintCommandRequest { commands })
    }

    fn upload_job(&self, lines: &[String]) -> Result<(), String> {
        let mut body = lines.join("\n");
        if !body.ends_with('\n') {
            body.push('\n');
        }

        let part = multipart::Part::bytes(body.into_bytes())
            .file_name(OCTOPRINT_JOB_FILE_NAME.to_owned())
            .mime_str("text/plain")
            .map_err(|error| {
                format!("{}: {error}", self.language.strings().octoprint_http_request_failed)
            })?;
        let form = multipart::Form::new()
            .text("select", "false")
            .text("print", "false")
            .part("file", part);

        self.send(self.request(Method::POST, "/api/files/local").multipart(form)).map(|_| ())
    }

    fn start_uploaded_job(&self) -> Result<(), String> {
        self.post_json(
            &format!("/api/files/local/{OCTOPRINT_JOB_FILE_NAME}"),
            &OctoPrintSelectRequest { command: "select", print: true },
        )
    }

    fn cancel_job(&self) -> Result<(), String> {
        self.post_json("/api/job", &OctoPrintJobRequest { command: "cancel" })
    }

    fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, String> {
        let response = self.send(self.request(Method::GET, path))?;
        response.json::<T>().map_err(|error| {
            format!("{}: {error}", self.language.strings().octoprint_http_request_failed)
        })
    }

    fn post_json<T: Serialize>(&self, path: &str, body: &T) -> Result<(), String> {
        self.send(self.request(Method::POST, path).json(body)).map(|_| ())
    }

    fn request(&self, method: Method, path: &str) -> reqwest::blocking::RequestBuilder {
        let url = format!("{}{}", self.base_url, path);
        let builder = self.client.request(method, url);
        if self.api_key.is_empty() { builder } else { builder.header("X-Api-Key", &self.api_key) }
    }

    fn send(
        &self,
        builder: reqwest::blocking::RequestBuilder,
    ) -> Result<reqwest::blocking::Response, String> {
        let response = builder.send().map_err(|error| {
            format!("{}: {error}", self.language.strings().octoprint_http_request_failed)
        })?;

        if response.status().is_success() {
            return Ok(response);
        }

        Err(self.error_from_status(response.status()))
    }

    fn error_from_status(&self, status: StatusCode) -> String {
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            self.language.strings().octoprint_auth_required.to_owned()
        } else {
            format!("{}: {status}", self.language.strings().octoprint_http_request_failed)
        }
    }
}

#[derive(Debug, Deserialize)]
struct OctoPrintVersionResponse {
    #[serde(default)]
    server: String,
    text: Option<String>,
}

impl OctoPrintVersionResponse {
    fn summary(&self) -> Option<String> {
        self.text
            .clone()
            .or_else(|| (!self.server.is_empty()).then(|| format!("OctoPrint {}", self.server)))
    }
}

#[derive(Debug, Deserialize)]
struct OctoPrintPrinterProfilesResponse {
    profiles: std::collections::BTreeMap<String, OctoPrintPrinterProfile>,
}

#[derive(Debug, Deserialize)]
struct OctoPrintPrinterProfile {
    #[serde(default)]
    current: bool,
    #[serde(default)]
    default: bool,
    volume: Option<OctoPrintProfileVolume>,
}

#[derive(Debug, Deserialize)]
struct OctoPrintProfileVolume {
    width: Option<f32>,
    depth: Option<f32>,
    units: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OctoPrintPrinterResponse {
    state: OctoPrintPrinterState,
}

#[derive(Debug, Deserialize)]
struct OctoPrintPrinterState {
    flags: OctoPrintPrinterFlags,
}

#[derive(Debug, Deserialize)]
struct OctoPrintPrinterFlags {
    #[serde(default)]
    operational: bool,
    #[serde(default)]
    printing: bool,
    #[serde(default)]
    paused: bool,
    #[serde(default)]
    pausing: bool,
    #[serde(default)]
    resuming: bool,
    #[serde(default)]
    finishing: bool,
    #[serde(default)]
    cancelling: bool,
    #[serde(default)]
    ready: bool,
}

impl OctoPrintPrinterFlags {
    fn is_ready_for_commands(&self) -> bool {
        self.operational
            || self.ready
            || self.printing
            || self.paused
            || self.pausing
            || self.resuming
            || self.finishing
            || self.cancelling
    }
}

#[derive(Debug, Serialize)]
struct OctoPrintCommandRequest<'a> {
    commands: &'a [String],
}

#[derive(Debug, Serialize)]
struct OctoPrintSelectRequest<'a> {
    command: &'a str,
    print: bool,
}

#[derive(Debug, Serialize)]
struct OctoPrintJobRequest<'a> {
    command: &'a str,
}

#[cfg(test)]
mod tests {
    use super::{
        OctoPrintPrinterProfile, OctoPrintPrinterProfilesResponse, OctoPrintProfileVolume,
        normalize_octoprint_base_url, printable_area_from_profiles,
    };
    use crate::res::lang::Language;

    #[test]
    fn normalizes_octoprint_base_url_and_adds_http() {
        let url = normalize_octoprint_base_url("192.168.0.110:5000/", Language::English).unwrap();
        assert_eq!(url, "http://192.168.0.110:5000");
    }

    #[test]
    fn reads_printable_area_from_current_profile() {
        let response = OctoPrintPrinterProfilesResponse {
            profiles: std::collections::BTreeMap::from([(
                "_default".to_owned(),
                OctoPrintPrinterProfile {
                    current: true,
                    default: true,
                    volume: Some(OctoPrintProfileVolume {
                        width: Some(220.0),
                        depth: Some(235.0),
                        units: None,
                    }),
                },
            )]),
        };

        let area = printable_area_from_profiles(&response).unwrap();
        assert_eq!(area.width_mm, 220.0);
        assert_eq!(area.height_mm, 235.0);
    }

    #[test]
    fn converts_printable_area_from_inches() {
        let response = OctoPrintPrinterProfilesResponse {
            profiles: std::collections::BTreeMap::from([(
                "_default".to_owned(),
                OctoPrintPrinterProfile {
                    current: true,
                    default: true,
                    volume: Some(OctoPrintProfileVolume {
                        width: Some(8.0),
                        depth: Some(8.0),
                        units: Some("in".to_owned()),
                    }),
                },
            )]),
        };

        let area = printable_area_from_profiles(&response).unwrap();
        assert_eq!(area.width_mm, 203.2);
        assert_eq!(area.height_mm, 203.2);
    }
}
