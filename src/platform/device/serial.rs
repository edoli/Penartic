use super::*;

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn run_serial_worker(
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

#[cfg(target_arch = "wasm32")]
pub(super) async fn run_web_serial_worker(
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
