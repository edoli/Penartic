# Penartic Design

## 1. Goal

Penartic is a Rust-based pen plotting and repurposed-3D-printer drawing application.
The app converts SVG input into an intermediate representation (IR) and then into motion/G-code,
previews the motion in a 3D viewport,
and can optionally stream the job to a connected serial device.

The product must remain useful even when no device is connected:

- users can import SVG files and inspect the generated drawing path offline
- printable area defaults are editable before a printer or plotter is attached
- connecting a device should update printable area information when firmware reports it

## 2. User-facing behavior

### 2.1 Offline workflow

1. Start the app without a device.
2. Set printable width, printable height, draw speed, Z lift height, optional G2/G3 or G5 output, and tangent-based corner-smoothing controls from the left sidebar. The default Z lift is 0.5 mm.
3. Load an SVG file through the file picker, drag-and-drop, or a native startup path used for validation.
4. Start from the SVG's raw coordinate size interpreted as millimeters, centered once on load, then adjust SVG position and size from the left sidebar when needed.
5. Convert the SVG into a reusable IR and then into preview motion plus G-code without automatically rescaling the existing SVG placement when printable area settings later change.
6. Show the completed result immediately in the 3D preview, then scrub backward or replay it with the timeline slider using real motion time.
7. Copy the generated G-code if needed.

### 2.2 Connected workflow

1. Refresh and select a serial port.
2. Connect to the device.
3. Probe firmware information (`M115`) and configuration (`M503`) on a best-effort basis.
4. If build volume information is detected, update the printable area and rebuild the toolpath without rewriting the current SVG placement or scale.
5. Choose whether print start should home XY first or begin directly from the current position; the default is direct-start without XY homing.
6. Use the built-in jog/home controls for XY and Z when manual positioning is needed, including a helper that raises Z by the configured lift amount, homes XY, and moves to the first drawing start point while lifted.
7. Queue the generated G-code to the device, stop it if needed, and keep invalid actions disabled while the current state is active.

### 2.3 Motion semantics

- continuous drawing moves stay on the XY plane at `Z = 0`
- travel moves lift the pen by the configured Z lift amount
- jobs in home-start mode lift Z with a relative move, home XY, move to the first drawing point while lifted, lower Z with a relative move, and then draw
- jobs in direct-start mode skip XY homing but still lift Z by the configured amount, move to the first drawing point while lifted, lower Z with a relative move, and then draw
- drawing moves within a stroke are emitted as continuous `G1` XY moves by default, can emit generated rounded-corner arcs as `G2`/`G3`, and can emit preserved Bézier segments or fallback rounded fillets as higher-level `G5` spline moves when the matching advanced modes are enabled

## 3. Runtime architecture

| Module | Responsibility |
| --- | --- |
| `src/gui/app.rs` | Main egui application state, sidebar UI, SVG loading, SVG placement controls, playback controls, and layout wiring |
| `src/gui/viewer.rs` | Custom WGPU paint callback for the bed, pen mesh, and timeline-aware motion preview |
| `src/gui/fonts.rs` | Native fallback CJK font discovery, deferred native font loading, and bundled web CJK font registration |
| `src/svg/ir.rs` | SVG intermediate-representation primitives, curve math, dash splitting, and polyline approximation helpers |
| `src/svg/toolpath.rs` | Parse SVG with `usvg`, build SVG IR strokes, compute intrinsic bounds, and apply persistent placement transforms |
| `src/plot/gcode.rs` | Convert SVG IR into preview motion segments, apply optional tangent-based join rounding, and emit linear, G2/G3, and/or G5 G-code |
| `src/plot/model.rs` | Shared settings, motion, and toolpath data structures |
| `src/platform/device.rs` | Native serial probing and streaming, plus native/web capability split |
| `src/platform/crash.rs` | Native panic hook and runtime error log persistence |
| `src/validation.rs` | Native UI screenshot validation wrapper that captures the egui viewport after a delay |
| `src/res/colors.rs` | Shared UI and preview color tokens |
| `src/lib.rs` / `src/main.rs` | Native/web bootstrap and platform-specific startup configuration |

## 4. Rendering design

### 4.1 UI layout

- left sidebar: fixed-width, vertically scrollable device controls, connection/print status, jog/home controls, editable print settings, job stats, warnings, logs
- sidebar action buttons use a slightly taller shared height, paired device/job actions are laid out in evenly sized columns with explicit spacing, the print-start homing toggle sits directly under the print action row, long firmware text stays on one line with hover access to the full value, device logs remain left-aligned, advanced G2/G3, G5, and corner-smoothing controls can be toggled from settings, and sidebar content growth must not resize the 3D preview when the window size stays fixed
- central panel: a full-size 3D preview canvas with a translucent bottom overlay for playback buttons and the full-width timeline slider so controls remain visible in smaller windows

### 4.2 3D preview

The preview uses an `egui_wgpu::CallbackTrait` paint callback instead of a separate rendering
window. The callback draws:

- the printable bed plane and grid
- completed draw/travel segments
- the current pen mesh at the playback position
- motion progress using elapsed toolpath time rather than raw segment count
- out-of-bounds SVG segments plus the placed SVG bounds when the drawing exceeds the printable area
- the default camera starts from a front-aligned orientation instead of a rotated diagonal view
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
- web builds register a bundled Noto Sans CJK KR font because egui renders text into its own
  canvas glyph atlas and cannot rely on page CSS fonts for Korean glyph coverage

## 6. Device integration

- serial support uses `serialport` on native builds and the browser Web Serial API on web builds
- the device controller keeps the app usable when no port is available
- firmware/build-volume probing is intentionally best-effort because printer responses vary by firmware
- native connection probing sends `M115`, `M503`, and `M211`; web connection probing starts with
  `M115` for readiness and then accepts the same firmware/build-volume response parsing while the
  serial stream is active; Marlin `M211` `Min:`/`Max:` reports are used to detect printable width and height when `M503` does not include build volume
- if device probing fails, the manually configured printable area remains authoritative
- detected printable area changes are applied only when the reported size actually changes, to avoid redundant rebuild churn
- printable area changes rebuild the preview/toolpath but do not overwrite a user-adjusted SVG placement or size
- printing state is tracked explicitly so start/stop/connect/disconnect controls can be enabled only when valid
- the UI keeps polling the native serial worker while a device is connected or connecting, so asynchronous probe responses can update settings after the initial click frame
- direct jog/home controls send synchronized metric movement commands for XY and Z when no print job is active
- a dedicated first-start-point command raises Z only by the configured lift amount, homes XY, and then moves to the first drawing start point without starting the whole job
- the serial worker strips comments before transmission, keeps a bounded set of acknowledged G-code lines in flight, and never treats read timeouts as acknowledgements
- web serial streaming follows the same comment stripping, ACK tracking, stop, jog/home, and bounded
  in-flight behavior as the native worker; it requires a browser with Web Serial support, a secure
  context, and the user's explicit port selection
- G2/G3 arc output is optional because firmware support varies and is used for rounded-corner transitions when those joins can be represented as true arcs
- G5 curve output is optional because firmware support varies; the default remains linear G-code for compatibility

## 7. SVG conversion pipeline

1. Parse SVG with `usvg`.
2. Walk visible path nodes.
3. Convert path segments into SVG IR strokes that preserve line, quadratic, and cubic geometry.
4. Capture stroke dash metadata so visible dashed spans can be generated from IR instead of being lost during parsing.
5. Compute intrinsic SVG bounds.
6. On load, create a one-time centered default placement that interprets SVG coordinate units as millimeters instead of auto-fitting to the printable area.
7. Reuse the user-controlled SVG placement for later rebuilds instead of auto-rescaling when printable area changes.
8. Apply placement and dash splitting in IR space.
9. Optionally replace sharp joins between adjacent primitives by comparing their end/start tangents and inserting a tiny rounded transition using a configurable radius and turn-angle threshold.
10. Mark drawings that extend beyond the printable area so the UI and preview can warn/highlight them.
11. Build preview motion segments with explicit travel lifts from the IR plus any generated rounded corners.
12. Emit standard linear G-code, optional `G2`/`G3` arc commands for compatible rounded corners, and optional `G5` curve commands for preserved Bézier geometry or fallback rounded fillets from the same pipeline.

Current non-goals:

- embedded raster images are not converted to toolpaths
- text nodes are not converted to strokes and are surfaced as warnings instead

## 8. Platform matrix

| Capability | Windows / macOS / Linux | Web |
| --- | --- | --- |
| SVG import and conversion | Yes | Yes |
| 3D preview | Yes | Yes |
| G-code copy/export flow | Yes | Yes |
| Serial device connection | Yes | Yes, through Web Serial |
| Firmware probing | Yes | Yes, best-effort through Web Serial |
| Local CJK font scanning | Yes | No, uses bundled CJK font |
| Crash log files | Yes | No |

## 9. Tooling and validation

- formatting: `cargo fmt`
- native validation: `cargo build`, `cargo test`
- web validation: `cargo build --target wasm32-unknown-unknown`
- SVG regression validation: load every file under `sample\*.svg` through the test suite
- native visual validation: run `cargo run --bin ui_screenshot_validation -- --svg sample\sample1.svg --out target\validation\ui-validation.png --delay-seconds 2`, then inspect the generated screenshot for obvious layout or rendering anomalies
- VS Code launch strategy:
  - Windows native debugging uses `cppvsdbg`
  - macOS/Linux native debugging uses `lldb`
  - web debugging uses `tools\run-web.ps1`, which installs `trunk` if needed and then runs `trunk serve --open`

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
- repo-local VS Code tasks provide build, test, SVG regression, web build, and Rust-driven native screenshot validation entry points
- the generated validation screenshot is currently written to `target\validation\ui-validation.png`
