# Penartic Design

## 1. Goal

Penartic is a Rust-based pen plotting and repurposed-3D-printer drawing application.
The app converts SVG input into an intermediate representation (IR) and then into motion/G-code,
previews the motion in a 3D viewport,
and can optionally send the job to a connected device over serial, ESP3D, or OctoPrint.

The product must remain useful even when no device is connected:

- users can import SVG files and inspect the generated drawing path offline
- printable area defaults are editable before a printer or plotter is attached
- connecting a device should update printable area information when firmware reports it

## 2. User-facing behavior

### 2.1 Offline workflow

1. Start the app without a device.
2. Choose the UI language from the left sidebar (default: English), then set printable width, printable height, draw speed, Z lift height, optional G2/G3 or G5 output, tangent-based corner-smoothing controls, and SVG fill controls. The default Z lift is 1.0 mm.
3. Load one or more SVG files through the file picker, drag-and-drop, or a native startup path used for validation.
4. Start each SVG from its raw coordinate size interpreted as millimeters, centered once on load, then select individual SVG objects and adjust position, independent X/Y scale, local width/height in millimeters, and rotation from the object toolbar or preview gizmo controls when needed.
5. Convert each SVG into reusable IR, combine the placed objects into one preview motion/G-code job, and avoid automatically rescaling existing SVG placements when printable area settings later change.
6. Show the completed result immediately in the 3D preview, then scrub backward or replay it with the timeline slider using real motion time.
7. Copy the generated G-code if needed.

### 2.2 Connected workflow

1. Choose the connection method, then enter the method-specific settings (serial port, ESP3D endpoint, or OctoPrint base URL and API key).
2. Connect to the device.
3. Probe firmware information (`M115`) and configuration (`M503`) on a best-effort basis.
4. If build volume information is detected, update the printable area and rebuild the toolpath without rewriting the current SVG placement or scale.
5. Choose whether print start should home XY first or begin directly from the current position; the default is direct-start without XY homing.
6. Use the built-in jog/home controls for XY and Z when manual positioning is needed, use Motors Off to release the steppers after manual setup, and use the dedicated positioning helpers to move directly to either the first drawing start point or the current timeline preview position with the same absolute XY motion flow.
7. Queue the generated G-code to the device, stop it if needed, and keep invalid actions disabled while the current state is active.

### 2.3 Motion semantics

- continuous drawing moves stay on the XY plane at `Z = 0`
- travel moves lift the pen by the configured Z lift amount
- jobs in home-start mode lift Z with a relative move, home XY, move to the first drawing point while lifted, lower Z with a relative move, and then draw
- jobs in direct-start mode skip XY homing but still lift Z by the configured amount, move to the first drawing point while lifted, lower Z with a relative move, and then draw
- drawing moves within a stroke are emitted as continuous `G1` XY moves by default; optional sharp-corner rounding is disabled by default, can emit generated rounded-corner arcs as `G2`/`G3` when enabled, and when G2/G3 output is enabled without G5 the app deduplicates the prepared stroke polyline and greedily compresses it into circular arcs and straight runs, but only accepts candidate arcs that satisfy seed/sagitta gates and stay close to the original polyline vertices plus segment midpoints before falling back to segmented `G1` output
- filled SVG paths are converted into internal fill strokes before G-code generation when fill support is enabled, including segmented hatch rows and continuous zigzag passes that keep the pen down across adjacent rows when possible

## 3. Runtime architecture

| Module | Responsibility |
| --- | --- |
| `src/gui/app.rs` | Main egui application state, sidebar UI, multi-SVG object loading/selection, placement controls, playback controls, and layout wiring |
| `src/gui/viewer.rs` | Custom WGPU paint callback for the bed, pen mesh, timeline-aware motion preview, and bed-plane object manipulation hit testing |
| `src/gui/fonts.rs` | Native fallback CJK font discovery, deferred native font loading, and bundled web CJK font registration |
| `src/res/lang/mod.rs` + `src/res/lang/english.rs` + `src/res/lang/korean.rs` | Shared language enum, localization abstraction, and concrete English/Korean string resources used across UI, SVG warnings, and device messaging |
| `src/paths/ir.rs` | Generic path intermediate-representation primitives, stroke/fill collection aliases, curve math, dash splitting, fill-region metadata, and polyline approximation helpers |
| `src/paths/stroke_processing.rs` | Generic stroke normalization, ordering, joining, and geometric bounds helpers used after parsing |
| `src/paths/svg_parser.rs` | Parse SVG with `usvg`, sample visible SVG paths into path IR, surface SVG-specific warnings, and apply persistent placement transforms |
| `src/plot/gcode.rs` | Convert IR into preview motion segments, generate fill hatch paths, apply optional tangent-based join rounding, and emit linear, G2/G3, and/or G5 G-code |
| `src/plot/model.rs` | Shared settings, motion, and toolpath data structures |
| `src/platform/device/mod.rs` | Transport-agnostic device controller state, connection preferences, worker dispatch, shared parsing/helpers, and device-facing app API |
| `src/platform/device/serial.rs` | Native serial and web-serial workers, ACK-driven streaming, and serial-specific probing |
| `src/platform/device/esp3d.rs` | Native/web ESP3D WebSocket or HTTP workers, queue management, and ESP3D endpoint normalization |
| `src/platform/device/octoprint.rs` | Native OctoPrint REST worker, printer/profile probing, command dispatch, job upload/start/cancel, and OctoPrint-specific state handling |
| `src/platform/crash.rs` | Native panic hook and runtime error log persistence |
| `src/validation.rs` | Native UI screenshot validation wrapper that captures the egui viewport after a delay |
| `src/res/colors.rs` | Shared UI and preview color tokens |
| `src/lib.rs` / `src/main.rs` | Native/web bootstrap and platform-specific startup configuration |

## 4. Rendering design

### 4.1 UI layout

- left sidebar: fixed-width, vertically scrollable language selector, device controls, connection-method selector, method-specific connection settings, connection/print status, jog/home controls, editable print settings, job stats, warnings, logs
- sidebar action buttons use a slightly taller shared height, paired device/job actions are laid out in evenly sized columns with explicit spacing, the print-start homing toggle sits directly under the print action row, long firmware text stays on one line with hover access to the full value, the upper sidebar controls scroll independently from a left-aligned device log section that fills the remaining sidebar height, advanced G2/G3, G5, corner-smoothing, and SVG fill controls can be toggled from settings, and sidebar content growth must not resize the 3D preview when the window size stays fixed
- central panel: a full-size 3D preview canvas with a translucent top object toolbar for move/scale/rotate selection, numeric X/Y position, independent X/Y scale percentages, local width/height millimeter edits, a default-on aspect-ratio lock for scale edits, rotation edits, and selected-object deletion; mode-specific gizmos provide move arrows, scale handles, or a rotation ring, and a translucent bottom overlay keeps playback buttons and the full-width timeline slider visible in smaller windows
- the preview overlay can command a connected idle device to move to the current timeline pen position and can toggle lifted travel paths or the placed SVG bounding box

### 4.2 3D preview

The preview uses an `egui_wgpu::CallbackTrait` paint callback instead of a separate rendering
window. The callback draws:

- the printable bed plane and grid
- completed draw segments, plus travel segments when the preview option to show lifted pen moves is enabled
- the current pen mesh at the playback position
- motion progress using elapsed toolpath time rather than raw segment count
- out-of-bounds SVG segments, plus the combined placed SVG bounding box when the matching preview option is enabled
- the default camera starts from a front-aligned orientation instead of a rotated diagonal view
- left drag rotates the camera unless it starts on a selectable SVG object; object drags manipulate the selected object according to the top toolbar mode, and right drag pans the view across the bed plane
- preview vertex buffers may grow when printable area or toolpath density changes and therefore must be resized safely before queue writes
- preview geometry is cached in world coordinates and transformed in the preview shader with a
  per-frame camera uniform, so panning/rotating/zooming complex SVGs does not rebuild or re-upload
  the full toolpath vertex buffer

### 4.3 WGPU/MSAA rule

The custom preview pipeline must use the same sample count as the enclosing eframe render pass.
If the app bootstrap changes native MSAA settings, the preview pipeline configuration must be
updated with the same value to avoid WGPU validation errors.

## 5. Fonts and localization

- the UI supports English and Korean, defaults to English, persists the selected language, the last-used device connection preferences, and the current G2/G3 or G5 curve-output selection through eframe storage on native and web builds, and sources localized strings from `src/res/lang/english.rs` and `src/res/lang/korean.rs`
- language persistence relies on `eframe`'s `persistence` feature, which maps to native app storage on desktop builds and browser local storage on web builds
- persisted device preferences include the selected connection method, serial port, ESP3D endpoint, and OctoPrint base URL/API key so the last-used device configuration is restored on restart, and the persisted tool settings currently keep the selected curve-output mode so G2/G3 and G5 toggles survive restart as well
- Korean mode still needs CJK-capable fallback fonts for the UI and warning text
- native builds asynchronously scan platform font locations and an optional `fallback_font.ttf`
  next to the executable
- loaded fallback fonts are appended to egui proportional and monospace families
- web builds register a bundled Noto Sans CJK KR font because egui renders text into its own
  canvas glyph atlas and cannot rely on page CSS fonts for Korean glyph coverage

## 6. Device integration

- device integration is transport-based: `DeviceController` keeps transport-agnostic UI state and dispatches work to dedicated serial, ESP3D, or OctoPrint workers
- serial support uses `serialport` on native builds and the browser Web Serial API on web builds
- native builds can alternatively connect through ESP3D over the Data WebSocket using the `arduino`
  subprotocol; `http://192.168.0.112/` is the default UI address and resolves to
  `ws://192.168.0.112:8282/`, while explicit `ws://...` addresses are used as-is; if the Data
  WebSocket is not enabled or rejects the `arduino` subprotocol, the worker falls back to the
  HTTP `/command` endpoint so ESP3D remains usable
- web builds can also connect through the ESP3D Data WebSocket using the same `arduino`
  subprotocol and endpoint normalization, but browsers block mixed-content `ws://` connections from
  an `https://` page, so hosted web builds require a `wss://` endpoint unless the app is served
  locally over `http://`/`localhost`
- native builds can also connect to OctoPrint with a base URL and API key; the worker verifies the server,
  reads the current printer profile for printable area when available, sends manual G-code through
  `/api/printer/command`, uploads generated jobs to `/api/files/local`, starts the uploaded file as a print,
  polls `/api/printer` and `/api/job` for readiness/progress, and cancels through `POST /api/job`
- OctoPrint is intentionally native-only for now; web builds expose the selector only for supported methods
- the device controller keeps the app usable when no port is available
- firmware/build-volume probing is intentionally best-effort because printer responses vary by firmware
- native serial connection probing sends `M115`, `M503`, and `M211`; native ESP3D probing sends the
  same probe commands through the WebSocket queue after the socket opens;
  web connection probing starts with
  `M115` for readiness and then accepts the same firmware/build-volume response parsing while the
  serial stream is active; web ESP3D probing sends `M115`, `M503`, and `M211` through the
  WebSocket queue after the socket opens; Marlin `M211` `Min:`/`Max:` reports are used to detect
  printable width and height when `M503` does not include build volume
- if device probing fails, the manually configured printable area remains authoritative
- detected printable area changes are applied only when the reported size actually changes, to avoid redundant rebuild churn
- printable area changes rebuild the preview/toolpath but do not overwrite user-adjusted SVG object placement, independent X/Y scale, local size, or rotation
- printing state is tracked explicitly so start/stop/connect/disconnect controls can be enabled only when valid
- the connection-method selector is only editable while disconnected, and each method shows only its own relevant settings inputs
- the UI keeps polling the native serial worker while a device is connected or connecting, so asynchronous probe responses can update settings after the initial click frame
- direct jog/home controls send synchronized metric movement commands for XY and Z when no print job is active
- the manual control UI also exposes a Motors Off action that waits for queued motion to finish and then sends `M84` to release the steppers
- dedicated positioning commands can move to the first drawing start point, current timeline preview position, or any placed SVG bounding-box corner without starting the whole job; the first-start helper now uses the same direct absolute XY move flow as timeline positioning
- the serial worker strips comments before transmission, sends one G-code line per firmware acknowledgement for conservative USB/firmware buffer handling, and never treats read timeouts as acknowledgements
- the ESP3D WebSocket worker strips comments through the shared queue, keeps a small bounded
  in-flight window so the firmware planner stays fed despite Wi-Fi/WebSocket latency, tops that
  window up in small increments instead of refilling it in one large burst, handles ESP3D binary text
  frames, tolerates compacted `ok` acknowledgement frames when deciding job completion, and sends
  `M410`/`M400` immediately when stopping a job
- web serial streaming follows the same comment stripping, ACK tracking, stop, jog/home, and bounded
  one-line in-flight behavior as the native worker; it requires a browser with Web Serial support, a secure
  context, and the user's explicit port selection
- G2/G3 arc output is optional because firmware support varies and is used both for rounded-corner transitions and for whole-stroke polyline arc compression when G5 is not selected, reducing overly segmented motion on printers that cannot execute `G5` while keeping whole-stroke fitting bounded by geometric-error checks against the original polyline
- G5 curve output is optional because firmware support varies; the default remains linear G-code for compatibility

## 7. SVG conversion pipeline

1. Parse SVG with `usvg`.
2. Walk visible path nodes.
3. Convert path segments into generic IR strokes that preserve line, quadratic, and cubic geometry.
4. Capture stroke dash metadata so visible dashed spans can be generated from IR instead of being lost during parsing.
5. Capture closed filled path contours as fill-region IR, including the SVG fill rule, so fill handling is separate from outline strokes.
6. Compute intrinsic SVG bounds from both stroke and fill contours.
7. On load, create a one-time centered default object placement that interprets SVG coordinate units as millimeters instead of auto-fitting to the printable area; later SVG imports append new objects instead of replacing existing ones.
8. Reuse each user-controlled SVG object placement for later rebuilds instead of auto-rescaling when printable area changes. The selected object can be deleted with Delete.
9. Convert raw SVG coordinates into source drawing space, split dashed strokes once, merge IR
   segments shorter than 0.5 mm into neighboring segments, drop strokes whose resulting length is
   still below 0.5 mm, then reorder strokes with a KD-tree-backed greedy nearest-endpoint pass and
   reverse individual strokes when their end point is closer than their start point. After ordering,
   adjacent strokes whose endpoint gap is 0.5 mm or smaller are joined into one continuous IR stroke.
10. Apply the current placement to the already ordered stroke and fill IR when SVG position, scale, or rotation changes, so
   placement rebuilds do not repeat dash splitting or stroke-order optimization.
11. Combine all placed objects into a single prepared drawing job while preserving per-object placement state for selection, warnings, and manipulation.
12. When fill support is enabled, generate pen fill strokes for filled regions before outline strokes. Fill patterns currently include single-direction lines, crosshatch, segmented zigzag rows, and continuous zigzag rows that greedily stitch adjacent scanlines when the connector stays inside the filled region. Fill density is exposed as a percentage and converted internally to spacing between successive fill passes, with higher density producing tighter spacing.
13. Optionally replace sharp joins between adjacent primitives by comparing their end/start tangents and inserting a tiny rounded transition using a configurable radius and turn-angle threshold.
14. Mark drawings that extend beyond the printable area so the UI and preview can warn/highlight them.
15. Build preview motion segments with explicit travel lifts from the IR plus any generated fill strokes and rounded corners.
16. Emit standard linear G-code, optional `G2`/`G3` arc commands for compatible rounded corners, and optional `G5` curve commands for preserved Bézier geometry or fallback rounded fillets from the same pipeline.
17. Filter non-finite drawing primitives before G-code generation and fall back to an absolute travel move instead of panicking if a Z-travel helper receives mismatched XY coordinates.

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
| ESP3D device connection | Yes | Yes |
| OctoPrint device connection | Yes | No |
| Firmware probing | Yes | Yes, best-effort through Web Serial |
| Local CJK font scanning | Yes | No, uses bundled CJK font |
| Crash log files | Yes | No |

## 9. Tooling and validation

- formatting: `cargo fmt`
- native validation: `cargo build`, `cargo test`
- web validation: `cargo build --target wasm32-unknown-unknown`
- SVG regression validation: load every file under `sample\*.svg` through the test suite
- native visual validation: run `cargo run --bin ui_screenshot_validation -- --svg sample\sample1.svg --out target\validation\ui-validation.png --delay-seconds 2`, then inspect the generated screenshot for obvious layout or rendering anomalies
- GitHub Pages deployment: pushes to `main` run `.github/workflows/deploy-pages.yml`, validate sample SVG loading, build the Trunk web bundle with a repository-relative public URL, and deploy the `dist` artifact through GitHub Pages Actions
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
