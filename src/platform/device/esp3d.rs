use super::*;

const ESP3D_DEFAULT_DATA_WEBSOCKET_PORT: u16 = 8282;

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn run_esp3d_worker(
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
pub(super) fn handle_esp3d_websocket_text(
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

pub(super) fn normalize_esp3d_endpoint(endpoint: &str) -> Result<String, String> {
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

#[cfg(target_arch = "wasm32")]
pub(super) async fn run_websocket_worker(
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
