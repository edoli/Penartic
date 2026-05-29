# Agent Instructions

## Documentation Sync

- When code changes affect architecture, workflows, supported platforms, settings, dependency/toolchain policy, validation steps, or other user-visible behavior, update `DESIGN.md` in the same task.

## Localization

- Do not hardcode user-facing UI strings in Rust UI code. Add strings to `src/res/lang/mod.rs` and provide matching English and Korean values in `src/res/lang/english.rs` and `src/res/lang/korean.rs`.
- Short machine-axis labels and icon-like glyphs may remain inline only when they are not natural-language UI text; add localized hover text for unfamiliar controls.

## SVG-specific Changes

- When modifying SVG loading, parsing, toolpath generation, preview behavior, or SVG-related G-code logic, validate that the sample SVG assets still load successfully.
- Use the repository SVG regression test path for this: `cargo test loads_all_sample_svg_assets_from_repository`.

## Validation

- After making code changes, run `cargo fmt`.
- After Rust code changes, run `cargo build`.
- After SVG-related Rust code changes, run `cargo test loads_all_sample_svg_assets_from_repository`.
- After UI layout, preview rendering/timeline behavior, SVG loading UX, or drag-and-drop changes, run the screenshot validation workflow: launch the app with `sample\sample_curve.svg`, wait briefly, capture a screenshot, and inspect it for obvious layout/rendering issues. On Windows, use the VS Code task `ui screenshot validation`.
- If `cargo build` is blocked because a running `penartic` process is holding the executable open (for example from a VS Code debug session), terminate the `penartic` process and retry the build.
