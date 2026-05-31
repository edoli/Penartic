use super::*;

use std::time::Duration;

use serde::{Deserialize, Serialize};
use url::Url;

#[cfg(not(target_arch = "wasm32"))]
use reqwest::{
    Method, StatusCode,
    blocking::{Client, multipart},
};

#[cfg(target_arch = "wasm32")]
use js_sys::{Array, Uint8Array};

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsValue;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen_futures::JsFuture;

#[cfg(target_arch = "wasm32")]
use web_sys::{AbortController, Blob, FormData, Request, RequestInit, RequestMode, Response};

#[cfg(not(target_arch = "wasm32"))]
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
        let mut last_progress_percent = None;
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
                        &mut last_progress_percent,
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
            if next_print_state == PrintState::Printing
                && observed_print_state != PrintState::Stopping
            {
                if let Ok(job) = service.job() {
                    if let Some(progress) = job.progress_fraction().and_then(|progress| {
                        next_progress_update(progress, &mut last_progress_percent)
                    }) {
                        event_tx
                            .send(WorkerEvent::JobProgress(progress))
                            .map_err(|error| error.to_string())?;
                    }
                }
            } else {
                last_progress_percent = None;
            }
            emit_print_state_transition(observed_print_state, next_print_state, &event_tx)?;
            observed_print_state = next_print_state;
        }
    })();

    if let Err(error) = result {
        let _ = event_tx.send(WorkerEvent::Error(error));
    }

    let _ = event_tx.send(WorkerEvent::Disconnected);
}

#[cfg(target_arch = "wasm32")]
pub(super) async fn run_web_worker(
    commands: WebCommandQueue,
    events: WebEventQueue,
    base_url: String,
    api_key: String,
    language: Language,
) {
    let result = run_web_worker_inner(&commands, &events, base_url, api_key, language).await;
    if let Err(error) = result {
        events.push(WorkerEvent::Error(error));
    }
    events.push(WorkerEvent::Disconnected);
}

#[cfg(target_arch = "wasm32")]
async fn run_web_worker_inner(
    commands: &WebCommandQueue,
    events: &WebEventQueue,
    base_url: String,
    api_key: String,
    language: Language,
) -> Result<(), String> {
    let service = WebOctoPrintService::new(&base_url, &api_key, language)?;
    let version = service.version().await?;
    let profiles = service.printer_profiles().await?;
    let printer = service.printer().await?;
    ensure_printer_ready(&printer, language)?;

    if let Some(summary) = version.summary() {
        events.push(WorkerEvent::FirmwareSummary(summary));
    }

    if let Some(area) = printable_area_from_profiles(&profiles) {
        events.push(WorkerEvent::DetectedArea(area));
    }

    events.push(WorkerEvent::Connected);
    events.push(WorkerEvent::Line(language.strings().octoprint_http_connected.to_owned()));

    let mut observed_print_state = print_state_from_printer(&printer);
    let mut last_progress_percent = None;
    if observed_print_state != PrintState::Idle {
        events.push(WorkerEvent::PrintStateChanged(observed_print_state));
    }

    loop {
        while let Some(command) = commands.pop() {
            if handle_web_worker_command(
                &service,
                command,
                events,
                &mut observed_print_state,
                &mut last_progress_percent,
            )
            .await?
            {
                return Ok(());
            }
        }

        let printer = service.printer().await?;
        ensure_printer_ready(&printer, language)?;
        let next_print_state = print_state_from_printer(&printer);
        if next_print_state == PrintState::Printing && observed_print_state != PrintState::Stopping
        {
            if let Ok(job) = service.job().await {
                if let Some(progress) = job
                    .progress_fraction()
                    .and_then(|progress| next_progress_update(progress, &mut last_progress_percent))
                {
                    events.push(WorkerEvent::JobProgress(progress));
                }
            }
        } else {
            last_progress_percent = None;
        }
        emit_print_state_transition_web(observed_print_state, next_print_state, events);
        observed_print_state = next_print_state;
        delay_ms(OCTOPRINT_POLL_INTERVAL.as_millis() as i32).await;
    }
}

#[cfg(target_arch = "wasm32")]
async fn handle_web_worker_command(
    service: &WebOctoPrintService,
    command: WorkerCommand,
    events: &WebEventQueue,
    observed_print_state: &mut PrintState,
    last_progress_percent: &mut Option<u8>,
) -> Result<bool, String> {
    if matches!(command, WorkerCommand::Disconnect) {
        return Ok(true);
    }

    process_web_worker_command(
        service,
        command,
        events,
        observed_print_state,
        last_progress_percent,
    )
    .await?;
    Ok(false)
}

#[cfg(target_arch = "wasm32")]
async fn process_web_worker_command(
    service: &WebOctoPrintService,
    command: WorkerCommand,
    events: &WebEventQueue,
    observed_print_state: &mut PrintState,
    last_progress_percent: &mut Option<u8>,
) -> Result<(), String> {
    match command {
        WorkerCommand::QueueJob(lines) => {
            let lines = clean_gcode_lines(lines);
            if lines.is_empty() {
                return Ok(());
            }

            *last_progress_percent = None;
            if let Err(error) = service.upload_job(&lines).await {
                events.push(WorkerEvent::JobFailed(error));
                return Ok(());
            }
            if let Err(error) = service.start_uploaded_job().await {
                events.push(WorkerEvent::JobFailed(error));
                return Ok(());
            }

            if *observed_print_state != PrintState::Printing {
                *observed_print_state = PrintState::Printing;
                events.push(WorkerEvent::PrintStateChanged(PrintState::Printing));
            }
        }
        WorkerCommand::QueueManual(lines) => {
            let lines = clean_gcode_lines(lines);
            if !lines.is_empty() {
                service.send_commands(&lines).await?;
            }
        }
        WorkerCommand::CancelJob => {
            *last_progress_percent = None;
            service.cancel_job().await?;
            if *observed_print_state != PrintState::Stopping {
                *observed_print_state = PrintState::Stopping;
                events.push(WorkerEvent::PrintStateChanged(PrintState::Stopping));
            }
        }
        WorkerCommand::Disconnect => {}
    }

    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn emit_print_state_transition_web(previous: PrintState, next: PrintState, events: &WebEventQueue) {
    if previous == next {
        return;
    }

    match (previous, next) {
        (PrintState::Printing, PrintState::Idle) => events.push(WorkerEvent::JobCompleted),
        (PrintState::Stopping, PrintState::Idle) => events.push(WorkerEvent::JobCancelled),
        (_, next_state) => events.push(WorkerEvent::PrintStateChanged(next_state)),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn handle_worker_command(
    service: &OctoPrintService,
    first_command: WorkerCommand,
    command_rx: &std::sync::mpsc::Receiver<WorkerCommand>,
    event_tx: &std::sync::mpsc::Sender<WorkerEvent>,
    observed_print_state: &mut PrintState,
    last_progress_percent: &mut Option<u8>,
) -> Result<bool, String> {
    if matches!(first_command, WorkerCommand::Disconnect) {
        return Ok(true);
    }
    process_worker_command(
        service,
        first_command,
        event_tx,
        observed_print_state,
        last_progress_percent,
    )?;
    while let Ok(command) = command_rx.try_recv() {
        if matches!(command, WorkerCommand::Disconnect) {
            return Ok(true);
        }
        process_worker_command(
            service,
            command,
            event_tx,
            observed_print_state,
            last_progress_percent,
        )?;
    }
    Ok(false)
}

#[cfg(not(target_arch = "wasm32"))]
fn process_worker_command(
    service: &OctoPrintService,
    command: WorkerCommand,
    event_tx: &std::sync::mpsc::Sender<WorkerEvent>,
    observed_print_state: &mut PrintState,
    last_progress_percent: &mut Option<u8>,
) -> Result<(), String> {
    match command {
        WorkerCommand::QueueJob(lines) => {
            let lines = clean_gcode_lines(lines);
            if lines.is_empty() {
                return Ok(());
            }

            *last_progress_percent = None;
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
            *last_progress_percent = None;
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

    fn job(&self) -> Result<OctoPrintJobResponse, String> {
        self.get_json("/api/job")
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

#[cfg(target_arch = "wasm32")]
struct WebOctoPrintService {
    base_url: String,
    api_key: String,
    language: Language,
}

#[cfg(target_arch = "wasm32")]
impl WebOctoPrintService {
    fn new(base_url: &str, api_key: &str, language: Language) -> Result<Self, String> {
        let base_url = normalize_octoprint_base_url(base_url, language)?;
        if web_page_uses_https() && base_url.starts_with("http://") {
            return Err(language.strings().secure_http_required.to_owned());
        }

        Ok(Self { base_url, api_key: api_key.trim().to_owned(), language })
    }

    async fn version(&self) -> Result<OctoPrintVersionResponse, String> {
        self.get_json("/api/version").await
    }

    async fn printer_profiles(&self) -> Result<OctoPrintPrinterProfilesResponse, String> {
        self.get_json("/api/printerprofiles").await
    }

    async fn printer(&self) -> Result<OctoPrintPrinterResponse, String> {
        self.get_json("/api/printer").await
    }

    async fn job(&self) -> Result<OctoPrintJobResponse, String> {
        self.get_json("/api/job").await
    }

    async fn send_commands(&self, commands: &[String]) -> Result<(), String> {
        self.post_json("/api/printer/command", &OctoPrintCommandRequest { commands }).await
    }

    async fn upload_job(&self, lines: &[String]) -> Result<(), String> {
        let mut body = lines.join("\n");
        if !body.ends_with('\n') {
            body.push('\n');
        }

        let parts = Array::new();
        parts.push(&Uint8Array::from(body.as_bytes()).into());
        let blob = Blob::new_with_u8_array_sequence(&parts).map_err(js_error_message)?;
        let form = FormData::new().map_err(js_error_message)?;
        form.append_with_blob_and_filename("file", &blob, OCTOPRINT_JOB_FILE_NAME)
            .map_err(js_error_message)?;
        form.append_with_str("select", "false").map_err(js_error_message)?;
        form.append_with_str("print", "false").map_err(js_error_message)?;
        self.send_request(MethodKind::Post, "/api/files/local", Some(form.into()), None)
            .await
            .map(|_| ())
    }

    async fn start_uploaded_job(&self) -> Result<(), String> {
        self.post_json(
            &format!("/api/files/local/{OCTOPRINT_JOB_FILE_NAME}"),
            &OctoPrintSelectRequest { command: "select", print: true },
        )
        .await
    }

    async fn cancel_job(&self) -> Result<(), String> {
        self.post_json("/api/job", &OctoPrintJobRequest { command: "cancel" }).await
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, String> {
        let response = self.send_request(MethodKind::Get, path, None, None).await?;
        let text = self.read_response_text(response).await?;
        serde_json::from_str(&text).map_err(|error| {
            format!("{}: {error}", self.language.strings().octoprint_http_request_failed)
        })
    }

    async fn post_json<T: Serialize>(&self, path: &str, body: &T) -> Result<(), String> {
        let body = serde_json::to_string(body).map_err(|error| {
            format!("{}: {error}", self.language.strings().octoprint_http_request_failed)
        })?;
        self.send_request(
            MethodKind::Post,
            path,
            Some(JsValue::from_str(&body)),
            Some("application/json"),
        )
        .await
        .map(|_| ())
    }

    async fn send_request(
        &self,
        method: MethodKind,
        path: &str,
        body: Option<JsValue>,
        content_type: Option<&str>,
    ) -> Result<Response, String> {
        let window = eframe::web_sys::window()
            .ok_or_else(|| self.language.strings().octoprint_http_request_failed.to_owned())?;
        let controller = AbortController::new().map_err(js_error_message)?;
        let init = RequestInit::new();
        init.set_method(method.as_str());
        init.set_mode(RequestMode::Cors);
        init.set_signal(Some(&controller.signal()));
        if let Some(body) = body.as_ref() {
            init.set_body(body);
        }
        let request = Request::new_with_str_and_init(&format!("{}{}", self.base_url, path), &init)
            .map_err(js_error_message)?;
        if let Some(content_type) = content_type {
            request.headers().set("Content-Type", content_type).map_err(js_error_message)?;
        }
        if !self.api_key.is_empty() {
            request.headers().set("X-Api-Key", &self.api_key).map_err(js_error_message)?;
        }

        let controller_for_timeout = controller.clone();
        let timeout_ms = OCTOPRINT_REQUEST_TIMEOUT.as_millis().min(i32::MAX as u128) as i32;
        spawn_local(async move {
            delay_ms(timeout_ms).await;
            controller_for_timeout.abort();
        });

        let response_value = JsFuture::from(window.fetch_with_request(&request))
            .await
            .map_err(|error| self.fetch_error_message(error, controller.signal().aborted()))?;
        let response: Response = response_value.dyn_into().map_err(js_error_message)?;
        if response.ok() { Ok(response) } else { Err(self.error_from_status(response.status())) }
    }

    async fn read_response_text(&self, response: Response) -> Result<String, String> {
        let text = JsFuture::from(response.text().map_err(js_error_message)?)
            .await
            .map_err(|error| self.fetch_error_message(error, false))?;
        text.as_string()
            .ok_or_else(|| self.language.strings().octoprint_http_request_failed.to_owned())
    }

    fn fetch_error_message(&self, error: JsValue, timed_out: bool) -> String {
        if timed_out {
            format!(
                "{}: {}",
                self.language.strings().octoprint_http_request_failed,
                self.language.strings().request_timed_out
            )
        } else {
            let detail = js_error_message(error);
            format!(
                "{}: {detail}. {}",
                self.language.strings().octoprint_http_request_failed,
                self.language.strings().octoprint_enable_cors_hint
            )
        }
    }

    fn error_from_status(&self, status: u16) -> String {
        if status == 401 || status == 403 {
            self.language.strings().octoprint_auth_required.to_owned()
        } else {
            format!("{}: {status}", self.language.strings().octoprint_http_request_failed)
        }
    }
}

#[cfg(target_arch = "wasm32")]
#[derive(Clone, Copy)]
enum MethodKind {
    Get,
    Post,
}

#[cfg(target_arch = "wasm32")]
impl MethodKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
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

#[derive(Debug, Default, Deserialize)]
struct OctoPrintJobResponse {
    #[serde(default)]
    progress: OctoPrintJobProgress,
}

impl OctoPrintJobResponse {
    fn progress_fraction(&self) -> Option<f32> {
        let completion = self.progress.completion?;
        completion.is_finite().then(|| (completion / 100.0).clamp(0.0, 1.0))
    }
}

#[derive(Debug, Default, Deserialize)]
struct OctoPrintJobProgress {
    completion: Option<f32>,
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
        OctoPrintJobProgress, OctoPrintJobResponse, OctoPrintPrinterProfile,
        OctoPrintPrinterProfilesResponse, OctoPrintProfileVolume, normalize_octoprint_base_url,
        printable_area_from_profiles,
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

    #[test]
    fn reads_job_progress_fraction_from_completion_percent() {
        let response =
            OctoPrintJobResponse { progress: OctoPrintJobProgress { completion: Some(42.5) } };

        let progress = response.progress_fraction().unwrap();
        assert!((progress - 0.425).abs() < f32::EPSILON);
    }
}
