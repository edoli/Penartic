#[cfg(not(target_arch = "wasm32"))]
use std::{
    backtrace::Backtrace,
    env, fs, panic,
    path::PathBuf,
    process,
    sync::Once,
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(not(target_arch = "wasm32"))]
static CRASH_HOOK_INIT: Once = Once::new();

#[cfg(not(target_arch = "wasm32"))]
pub fn install_crash_logging() {
    CRASH_HOOK_INIT.call_once(|| {
        let default_hook = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            let report = build_panic_report(info);
            if let Some(path) = write_report("panic", &report) {
                eprintln!("Penartic crash log: {}", path.display());
            }

            default_hook(info);
        }));
    });
}

#[cfg(target_arch = "wasm32")]
pub fn install_crash_logging() {}

#[cfg(not(target_arch = "wasm32"))]
pub fn log_runtime_error(context: &str, message: &str) {
    let report = format!(
        "Penartic runtime error\ncontext: {context}\ntimestamp_unix: {}\nprocess_id: {}\n\nmessage:\n{message}\n",
        unix_timestamp(),
        process::id(),
    );

    if let Some(path) = write_report("runtime-error", &report) {
        eprintln!("Penartic runtime error log: {}", path.display());
    }
}

#[cfg(target_arch = "wasm32")]
pub fn log_runtime_error(_context: &str, _message: &str) {}

#[cfg(not(target_arch = "wasm32"))]
fn build_panic_report(info: &panic::PanicHookInfo<'_>) -> String {
    let thread_name = thread::current().name().unwrap_or("unnamed").to_owned();
    let payload = if let Some(message) = info.payload().downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = info.payload().downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_owned()
    };
    let location = info
        .location()
        .map(|location| format!("{}:{}:{}", location.file(), location.line(), location.column()))
        .unwrap_or_else(|| "unknown".to_owned());
    let args = env::args().collect::<Vec<_>>().join(" ");

    format!(
        "Penartic panic report\n\
timestamp_unix: {}\n\
process_id: {}\n\
thread: {}\n\
location: {}\n\
args: {}\n\n\
message:\n{}\n\n\
backtrace:\n{}\n",
        unix_timestamp(),
        process::id(),
        thread_name,
        location,
        args,
        payload,
        Backtrace::force_capture(),
    )
}

#[cfg(not(target_arch = "wasm32"))]
fn write_report(prefix: &str, report: &str) -> Option<PathBuf> {
    let dir = report_dir();
    fs::create_dir_all(&dir).ok()?;

    let file_name = format!("{prefix}-{}-pid{}.log", unix_timestamp(), process::id());
    let path = dir.join(file_name);
    fs::write(&path, report).ok()?;
    Some(path)
}

#[cfg(not(target_arch = "wasm32"))]
fn report_dir() -> PathBuf {
    if let Some(override_dir) = env::var_os("PENARTIC_CRASH_LOG_DIR") {
        return PathBuf::from(override_dir);
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
            return PathBuf::from(local_app_data).join("Penartic").join("crash-logs");
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(home) = env::var_os("HOME") {
            return PathBuf::from(home).join("Library").join("Logs").join("Penartic");
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Some(state_home) = env::var_os("XDG_STATE_HOME") {
            return PathBuf::from(state_home).join("penartic").join("crash-logs");
        }
        if let Some(home) = env::var_os("HOME") {
            return PathBuf::from(home)
                .join(".local")
                .join("state")
                .join("penartic")
                .join("crash-logs");
        }
    }

    env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join("crash-logs")
}

#[cfg(not(target_arch = "wasm32"))]
fn unix_timestamp() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}
