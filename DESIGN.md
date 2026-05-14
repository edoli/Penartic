# Penartic Design

## 1. Goal

Penartic is a Rust-based pen plotting and repurposed-3D-printer drawing application.
The app converts SVG input into a toolpath and G-code, previews the motion in a 3D viewport,
and can optionally stream the job to a connected serial device.

The product must remain useful even when no device is connected:

- users can import SVG files and inspect the generated drawing path offline
- printable area defaults are editable before a printer or plotter is attached
- connecting a device should update printable area information when firmware reports it

## 2. User-facing behavior

### 2.1 Offline workflow

1. Start the app without a device.
2. Set printable width, printable height, draw speed, and Z lift height from the left sidebar.
3. Load an SVG file through the file picker, drag-and-drop, or a native startup path used for validation.
4. Convert the SVG into a sampled toolpath and G-code.
5. Show the completed result immediately in the 3D preview, then scrub backward or replay it with the timeline slider using real motion time.
6. Copy the generated G-code if needed.

### 2.2 Connected workflow

1. Refresh and select a serial port.
2. Connect to the device.
3. Probe firmware information (`M115`) and configuration (`M503`) on a best-effort basis.
4. If build volume information is detected, update the printable area and rebuild the toolpath.
5. Use the built-in jog/home controls for XY and Z when manual positioning is needed.
6. Queue the generated G-code to the device, stop it if needed, and keep invalid actions disabled while the current state is active.

### 2.3 Motion semantics

- continuous drawing moves stay on the XY plane at `Z = 0`
- travel moves lift the pen by the configured Z lift amount
- generated jobs start by lifting Z and then homing XY before drawing begins

## 3. Runtime architecture

| Module | Responsibility |
| --- | --- |
| `src/gui/app.rs` | Main egui application state, sidebar UI, SVG loading, playback controls, and layout wiring |
| `src/gui/viewer.rs` | Custom WGPU paint callback for the bed, pen mesh, and timeline-aware motion preview |
| `src/gui/fonts.rs` | Native fallback CJK font discovery and deferred font loading |
| `src/svg/toolpath.rs` | Parse SVG with `usvg`, flatten path segments into polylines, normalize into printable space |
| `src/plot/gcode.rs` | Convert sampled polylines into travel/draw motion segments and G-code |
| `src/plot/model.rs` | Shared settings, motion, and toolpath data structures |
| `src/platform/device.rs` | Native serial probing and streaming, plus native/web capability split |
| `src/platform/crash.rs` | Native panic hook and runtime error log persistence |
| `src/res/colors.rs` | Shared UI and preview color tokens |
| `src/lib.rs` / `src/main.rs` | Native/web bootstrap and platform-specific startup configuration |

## 4. Rendering design

### 4.1 UI layout

- left sidebar: fixed-width device controls, connection/print status, jog/home controls, editable print settings, job stats, warnings, logs
- central panel: preview title, 3D preview canvas, and a bottom control band reserved for playback buttons plus a full-width timeline slider

### 4.2 3D preview

The preview uses an `egui_wgpu::CallbackTrait` paint callback instead of a separate rendering
window. The callback draws:

- the printable bed plane and grid
- completed draw/travel segments
- the current pen mesh at the playback position
- motion progress using elapsed toolpath time rather than raw segment count
- left drag rotates the camera and right drag pans the view across the bed plane
- preview vertex buffers may grow when printable area or toolpath density changes and therefore must be resized safely before queue writes

### 4.3 WGPU/MSAA rule

The custom preview pipeline must use the same sample count as the enclosing eframe render pass.
If the app bootstrap changes native MSAA settings, the preview pipeline configuration must be
updated with the same value to avoid WGPU validation errors.

## 5. Fonts and localization

- UI strings currently include Korean text and therefore need CJK-capable fallback fonts
- native builds asynchronously scan platform font locations and an optional `fallback_font.ttf`
  next to the executable
- loaded fallback fonts are appended to egui proportional and monospace families
- web builds currently rely on browser/system fonts and do not scan local files

## 6. Device integration

- serial support is native-only and uses `serialport`
- the device controller keeps the app usable when no port is available
- firmware/build-volume probing is intentionally best-effort because printer responses vary by firmware
- if device probing fails, the manually configured printable area remains authoritative
- detected printable area changes are applied only when the reported size actually changes, to avoid redundant rebuild churn
- printing state is tracked explicitly so start/stop/connect/disconnect controls can be enabled only when valid
- direct jog/home controls send synchronized metric movement commands for XY and Z when no print job is active
- the serial worker keeps many G-code lines in flight, batches writes, and does not flush the serial port after every line, so typical planner-based firmware can move more smoothly than a strict one-line-at-a-time stream

## 7. SVG conversion pipeline

1. Parse SVG with `usvg`.
2. Walk visible path nodes.
3. Convert path segments into polyline samples.
4. Compute drawing bounds.
5. Fit and center the drawing into the configured printable area.
6. Build motion segments with explicit travel lifts.
7. Emit G-code and preview data from the same toolpath plan.

Current non-goals:

- embedded raster images are not converted to toolpaths
- text nodes are not converted to strokes and are surfaced as warnings instead

## 8. Platform matrix

| Capability | Windows / macOS / Linux | Web |
| --- | --- | --- |
| SVG import and conversion | Yes | Yes |
| 3D preview | Yes | Yes |
| G-code copy/export flow | Yes | Yes |
| Serial device connection | Yes | No |
| Firmware probing | Yes | No |
| Local CJK font scanning | Yes | No |
| Crash log files | Yes | No |

## 9. Tooling and validation

- formatting: `cargo fmt`
- native validation: `cargo build`, `cargo test`
- web validation: `cargo build --target wasm32-unknown-unknown`
- SVG regression validation: load every file under `sample\*.svg` through the test suite
- native visual validation: launch the app with `sample\sample1.svg`, wait briefly, capture a screenshot, and inspect it for obvious layout or rendering anomalies
- VS Code launch strategy:
  - Windows native debugging uses `cppvsdbg`
  - macOS/Linux native debugging uses `lldb`
  - web debugging uses `trunk serve --open`

## 10. Dependency and toolchain policy

- prefer the latest stable crate releases that are compatible with the repository toolchain
- the repository is pinned to Rust 1.92 so the current `eframe`/`wgpu` stack can track the latest stable release line
- if a dependency upgrade requires a newer Rust toolchain, update the repository toolchain pin
  together with the dependency change
- when architecture, validation steps, platform behavior, or dependency policy changes, this file
  must be updated in the same task

## 11. Operational artifacts

- sample SVG assets used for regression live under `sample\`
- native crash logs are written to a platform-specific application log directory
- repo-local VS Code tasks provide build, test, SVG regression, web build, and Windows screenshot validation entry points
- the generated validation screenshot is currently written to `target\validation\ui-validation.png`
