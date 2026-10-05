// SPDX-License-Identifier: MIT

fn main() {
    // Debug builds carry element names and types, so tests can find elements with `i-slint-backend-testing`.
    let debug = std::env::var("PROFILE").is_ok_and(|profile| profile == "debug");
    let config = slint_build::CompilerConfiguration::new()
        .with_library_paths(nimbus_theme::library_paths())
        .with_debug_info(debug);
    slint_build::compile_with_config("ui/shell.slint", config).unwrap();
}
