// SPDX-License-Identifier: MIT

use std::path::{Path, PathBuf};

use slint_interpreter::{CompilationResult, Compiler, DiagnosticLevel};

fn ui_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("ui")
}

fn compiler() -> Compiler {
    let mut compiler = Compiler::default();
    compiler.set_library_paths(nimbus_theme::library_paths());
    compiler
}

/// Returns the diagnostics of `result` as `(errors, warnings)`.
fn diagnostics(result: &CompilationResult) -> (Vec<String>, Vec<String>) {
    let (errors, warnings): (Vec<_>, Vec<_>) =
        result.diagnostics().partition(|d| d.level() == DiagnosticLevel::Error);
    let text =
        |list: Vec<slint_interpreter::Diagnostic>| list.iter().map(ToString::to_string).collect();
    (text(errors), text(warnings))
}

/// Every name `theme.slint` exports, as a consumer imports them.
const EXPORTS: &str = "Theme, Motion, TextStyle, Shadow, ButtonVariant, Tone, Icons, Icon, Label, FocusRing, \
    Button, IconButton, ToggleSwitch, CheckBox, Slider, SegmentedControl, Dropdown, Card, Popover, ListRow, \
    TextField, SearchField, Badge, ProgressBar, Divider, Spinner, NavigationItem, TabBar, HeaderBar";

#[test]
fn theme_library_compiles_without_warnings() {
    let source = format!(
        "import {{ {EXPORTS} }} from \"@nimbus/theme.slint\";\n\
         export {{ Theme }}\n\
         export component App inherits Window {{ background: Theme.background; }}\n"
    );
    let result = spin_on::spin_on(compiler().build_from_source(source, PathBuf::from("app.slint")));
    let (errors, warnings) = diagnostics(&result);
    assert!(errors.is_empty() && warnings.is_empty(), "{errors:#?}\n{warnings:#?}");
}

#[test]
fn gallery_compiles() {
    let path = ui_dir().join("gallery.slint");
    let result = spin_on::spin_on(compiler().build_from_path(&path));
    let (errors, warnings) = diagnostics(&result);
    assert!(errors.is_empty(), "{}:\n{}", path.display(), errors.join("\n"));
    assert!(warnings.is_empty(), "{}:\n{}", path.display(), warnings.join("\n"));
    assert!(result.component("Gallery").is_some());
}

#[test]
fn every_icon_file_is_exported() {
    let icons =
        std::fs::read_to_string(ui_dir().join("icons.slint")).expect("icons.slint is readable");
    let mut count = 0;
    for entry in std::fs::read_dir(ui_dir().join("icons")).expect("icons/ is readable") {
        let path = entry.expect("directory entry").path();
        let name = path.file_stem().and_then(|s| s.to_str()).expect("UTF-8 file name");
        assert!(
            icons.contains(&format!("@image-url(\"icons/{name}.svg\")")),
            "{name} isn't in Icons"
        );
        count += 1;
    }
    assert!(count >= 100, "only {count} icons");
}

#[test]
fn consumers_import_through_the_library_path() {
    let source = r#"
        import { Theme, Button, Icons, Icon } from "@nimbus/theme.slint";
        export component App inherits Window {
            background: Theme.background;
            Button { text: "OK"; icon: Icons.check; variant: primary; }
        }
    "#;
    let result =
        spin_on::spin_on(compiler().build_from_source(source.into(), PathBuf::from("app.slint")));
    let (errors, _) = diagnostics(&result);
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    assert!(result.component("App").is_some());
}
