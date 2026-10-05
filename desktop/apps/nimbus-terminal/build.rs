// SPDX-License-Identifier: MIT

fn main() {
    let config = slint_build::CompilerConfiguration::new().with_library_paths(nimbus_theme::library_paths());
    slint_build::compile_with_config("ui/main.slint", config).unwrap();
}
