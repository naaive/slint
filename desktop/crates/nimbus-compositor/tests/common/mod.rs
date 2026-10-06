// SPDX-License-Identifier: MIT

//! Starts this package's compositor headless; see `nimbus-test-support` for the harness.

#![allow(dead_code)]

pub use nimbus_test_support::*;

/// Prepares this package's compositor with `config`.
pub fn compositor(config: &str) -> CompositorBuilder {
    Compositor::builder(config).binary(env!("CARGO_BIN_EXE_nimbus-compositor"))
}

/// Starts this package's compositor with `config` and the default output.
pub fn start(config: &str) -> Compositor {
    compositor(config).start()
}
