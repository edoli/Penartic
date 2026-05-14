#[cfg(not(target_arch = "wasm32"))]
use std::{env, error::Error, path::PathBuf, time::Duration};

#[cfg(not(target_arch = "wasm32"))]
use penartic_app::{NativeScreenshotValidationConfig, run_native_screenshot_validation};

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), Box<dyn Error>> {
    let config = parse_args()?;
    run_native_screenshot_validation(config)
}

#[cfg(target_arch = "wasm32")]
fn main() {
    panic!("ui_screenshot_validation is only available on native targets");
}

#[cfg(not(target_arch = "wasm32"))]
fn parse_args() -> Result<NativeScreenshotValidationConfig, Box<dyn Error>> {
    let mut config = NativeScreenshotValidationConfig::default();
    let mut args = env::args_os().skip(1);

    while let Some(arg) = args.next() {
        match arg.to_string_lossy().as_ref() {
            "--svg" => {
                config.startup_svg_path = next_path(&mut args, "--svg")?;
            }
            "--out" => {
                config.output_path = next_path(&mut args, "--out")?;
            }
            "--delay-seconds" => {
                config.delay = Duration::from_secs_f32(next_f32(&mut args, "--delay-seconds")?);
            }
            "--timeout-seconds" => {
                config.timeout = Duration::from_secs_f32(next_f32(&mut args, "--timeout-seconds")?);
            }
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            other => {
                return Err(format!("unknown argument: {other}").into());
            }
        }
    }

    Ok(config)
}

#[cfg(not(target_arch = "wasm32"))]
fn next_path(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
    name: &str,
) -> Result<PathBuf, Box<dyn Error>> {
    args.next().map(PathBuf::from).ok_or_else(|| format!("missing value for {name}").into())
}

#[cfg(not(target_arch = "wasm32"))]
fn next_f32(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
    name: &str,
) -> Result<f32, Box<dyn Error>> {
    let value = args
        .next()
        .ok_or_else(|| format!("missing value for {name}"))?
        .to_string_lossy()
        .parse::<f32>()?;

    if value.is_sign_negative() {
        return Err(format!("{name} must be non-negative").into());
    }

    Ok(value)
}

#[cfg(not(target_arch = "wasm32"))]
fn print_help() {
    println!(
        "Usage: cargo run --bin ui_screenshot_validation -- [--svg sample/sample1.svg] [--out target/validation/ui-validation.png] [--delay-seconds 2] [--timeout-seconds 20]"
    );
}
