// SPDX-License-Identifier: MIT

use nimbus_theme::headless::Headless;
use slint::platform::software_renderer::PremultipliedRgbaColor;
use slint_interpreter::ComponentHandle;

const SOURCE: &str = r#"
export component Probe inherits Window {
    background: transparent;
    Rectangle { x: 0; y: 0; width: 4px; height: 3px; background: #ff8000; }
    Rectangle { x: 4px; y: 0; width: 4px; height: 3px; background: #0080ff80; }
}
"#;

fn probe() -> slint_interpreter::ComponentInstance {
    let compiler = slint_interpreter::Compiler::default();
    let result = spin_on::spin_on(compiler.build_from_source(SOURCE.into(), Default::default()));
    assert!(!result.has_errors(), "{:#?}", result.diagnostics().collect::<Vec<_>>());
    result.component("Probe").expect("Probe is exported").create().expect("Probe instantiates")
}

#[test]
fn renders_frames_and_writes_png() {
    let headless = Headless::install(10, 5).expect("no platform was set on this thread");
    assert!(Headless::install(10, 5).is_err(), "the platform is set once per thread");
    let probe = probe();
    probe.show().expect("the window shows");

    let frame = headless.render();
    assert_eq!((frame.width, frame.height, frame.pixels.len()), (10, 5, 50));
    assert_eq!(frame.pixel(0, 0), Some((0xff, 0x80, 0x00)));
    assert_eq!(frame.pixel(9, 4), Some((0, 0, 0)), "uncovered pixels are black");
    assert_eq!((frame.pixel(10, 0), frame.pixel(0, 5)), (None, None));
    assert_eq!(frame.distinct_colors(), 3);
    assert_eq!(headless.draw(), frame);

    let rgba: Vec<PremultipliedRgbaColor> = headless.render_pixels();
    assert_eq!(rgba[0].alpha, 0xff);
    assert_eq!((rgba[4].alpha, rgba[9].alpha), (0x80, 0));

    let dir = tempfile::tempdir().expect("temporary directory");
    let path = dir.path().join("nested/frame.png");
    frame.write_png(&path).expect("the PNG is written");
    let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&path).unwrap()));
    let mut reader = decoder.read_info().expect("the PNG decodes");
    let mut bytes = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut bytes).expect("the PNG has a frame");
    assert_eq!((info.width, info.height, info.color_type), (10, 5, png::ColorType::Rgb));
    let expected: Vec<u8> = frame.pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
    assert_eq!(&bytes[..info.buffer_size()], expected);
    probe.hide().expect("the window hides");
}

#[test]
fn focus_ring_surrounds_its_parent() {
    let source = r#"
        import { FocusRing } from "@nimbus/theme.slint";
        export component Probe inherits Window {
            background: black;
            Rectangle {
                x: 10px;
                y: 6px;
                width: 20px;
                height: 8px;
                FocusRing { radius: 0; offset: 2px; }
            }
        }
    "#;
    let mut compiler = slint_interpreter::Compiler::default();
    compiler.set_library_paths(nimbus_theme::library_paths());
    let result = spin_on::spin_on(compiler.build_from_source(source.into(), Default::default()));
    assert!(!result.has_errors(), "{:#?}", result.diagnostics().collect::<Vec<_>>());
    let headless = Headless::install(40, 20).expect("no platform was set on this thread");
    let probe = result.component("Probe").unwrap().create().unwrap();
    probe.show().expect("the window shows");

    let frame = headless.render();
    let black = Some((0, 0, 0));
    for (x, y) in [(8, 10), (31, 10), (20, 4), (20, 15)] {
        assert_ne!(frame.pixel(x, y), black, "no ring at ({x}, {y})");
    }
    assert_eq!(frame.pixel(20, 10), black, "the ring is filled");
    assert_eq!(frame.pixel(5, 10), black, "the ring is too wide");
    probe.hide().expect("the window hides");
}
