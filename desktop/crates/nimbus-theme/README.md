<!-- SPDX-License-Identifier: MIT -->

# nimbus-theme

The Nimbus design system: color, spacing, type, and motion tokens, a set of Slint components, and symbolic icons.
The shell and every Nimbus app import it, so they look like one product.

![Gallery, dark](../../docs/screenshots/theme-gallery-dark.png)

## Using It

Wire the library path in `build.rs`:

```rust
let config = slint_build::CompilerConfiguration::new().with_library_paths(nimbus_theme::library_paths());
slint_build::compile_with_config("ui/main.slint", config).unwrap();
```

Import everything from one file, and export `Theme` so Rust can set it:

```slint
import { Theme, Button, Icons } from "@nimbus/theme.slint";
export { Theme }

export component MainWindow inherits Window {
    background: Theme.background;
    default-font-family: Theme.font-family;
    default-font-size: Theme.font-size;
    Button { text: "Save"; icon: Icons.check; variant: primary; }
}
```

Apply the user's settings at startup and whenever the configuration changes:

```rust
slint::include_modules!();
let ui = MainWindow::new()?;
nimbus_theme::apply_theme!(ui, nimbus_theme::ThemeSettings::from_config(&config.appearance));
```

`apply_theme!` uses the `Theme` type in scope at the call site.
Pass a path as a third argument when it isn't in scope: `apply_theme!(ui, settings, ui::Theme)`.

`ThemeSettings::from_config` resolves `color-scheme = "system"` through the XDG desktop portal,
with a timeout of about a second, and falls back to dark.
It converts the configured font size from points to logical pixels.

## Tokens

All tokens live in the `Theme` global.
The inputs are `in-out`; everything else is derived from them.

| Group | Properties |
| --- | --- |
| Inputs | `dark`, `accent`, `corner-radius`, `font-family`, `font-size`, `animations` |
| Surfaces | `background`, `surface`, `surface-raised`, `surface-sunken`, `surface-hover`, `surface-pressed`, `surface-selected`, `overlay`, `panel-background`, `popup-background` |
| Controls | `control`, `control-hover`, `control-pressed` |
| Content | `text`, `text-secondary`, `text-disabled`, `border`, `border-strong`, `focus-ring` |
| Accent | `accent-hover`, `accent-pressed`, `accent-text`, `on-accent` |
| Status | `success`, `warning`, `error`, `on-status`, `destructive`, `destructive-hover`, `destructive-pressed`, `on-destructive` |
| Spacing | `spacing-xxs` (2px), `spacing-xs` (4), `spacing-sm` (8), `spacing-md` (12), `spacing-lg` (16), `spacing-xl` (24), `spacing-xxl` (32) |
| Radii | `radius-small`, `radius-medium`, `radius-large` (= `corner-radius`), `radius-xlarge`, `radius-pill` |
| Sizes | `control-height`, `control-height-small`, `row-height`, `header-bar-height`, `icon-size`, `icon-size-large`, `focus-ring-width` |
| Type | `caption`, `body`, `body-strong`, `title`, `headline`, `display`, each a `TextStyle { font-size, font-weight }` |
| Elevation | `elevation-1`, `elevation-2`, `elevation-3`, each a `Shadow { blur, offset-y, color }` |
| Motion | `duration-fast`, `duration-normal`, `duration-slow`, all zero when `animations` is false |

Use `accent-text` for accent-colored text and icons on a surface, and `accent` for fills.
Use `error` for text and icons, and `destructive` for fills.
`panel-background` and `popup-background` are translucent.

Easing curves are in the separate `Motion` global (`standard`, `enter`, `exit`),
because Slint can't expose `easing` properties to Rust:

```slint
animate background { duration: Theme.duration-fast; easing: Motion.standard; }
```

## Components

| Component | Purpose |
| --- | --- |
| `Label` | Text in the type scale: `Label { text: "Wi-Fi"; style: Theme.title; }`. |
| `Icon` | A symbolic image tinted with `color`, sized by `size`. |
| `Button` | Push button with `text` and `icon`; `variant` is `default`, `primary`, `flat`, or `destructive`; `compact` for dense UIs. |
| `IconButton` | Round icon-only button; `checked` for toggles, `filled` for a resting fill. |
| `ToggleSwitch` | On/off switch with `checked` and `toggled(bool)`. |
| `CheckBox` | Check box with optional `text`. |
| `Slider` | Horizontal slider with optional leading `icon`; `changed(value)` while dragging, `released(value)` at the end. |
| `SegmentedControl` | Two to five exclusive options from a `[string]` model. |
| `Dropdown` | A button opening a popup list; `selected(index, value)`. |
| `TabBar` | Tabs with an accent underline. |
| `Card` | Rounded surface; children stack vertically with `content-padding` and `spacing`; `elevated`, `clickable`. |
| `Popover` | Floating surface for menus and notifications. |
| `ListRow` | Icon, title, subtitle, and trailing children; place rows in a `Card`. |
| `TextField` | Single-line input with `placeholder`, `password`, `invalid`, and an optional `icon`. |
| `SearchField` | Pill-shaped search input with a clear button; Escape clears it. |
| `Badge` | Count or status pill in a `Tone`; a dot when `text` is empty. |
| `ProgressBar` | Determinate `progress` from 0 to 1, or `indeterminate`. |
| `Spinner` | Rotating activity indicator. |
| `Divider` | Thin separator; set `vertical` in horizontal layouts. |
| `NavigationItem` | Sidebar entry for settings-style apps, with `selected` and an optional `badge`. |
| `HeaderBar` | App title area with a centered title; children sit at the trailing end, and `drag-started` and `double-clicked` let the window move and maximize. |
| `FocusRing` | The keyboard focus indicator used by the components, for custom controls. |

Interactive components show the focus ring only for keyboard focus.
Use the std-widgets `ScrollView` and `ListView` for scrolling.

## Icons

The `Icons` global has one `image` property per icon, such as `Icons.wifi-2` or `Icons.battery-charging`.
They're 16x16 symbolic SVGs, so show them with `Icon` to tint them.
Levels come in numbered sets: `wifi-0` to `wifi-3`, `battery-0` (empty) to `battery-4` (full), and `volume-muted`, `-low`, `-medium`, `-high`.

The SVGs and `ui/icons.slint` are generated.
To add or change an icon, edit the shapes in `tools/icons.py` and run `python3 tools/icons.py` from this directory.

## Testing

`cargo test -p nimbus-theme` compiles the library with the interpreter and the Rust code generator,
and renders `ui/gallery.slint` in both schemes with the software renderer.
Set `NIMBUS_UPDATE_SCREENSHOTS=1` to write `docs/screenshots/theme-gallery-dark.png` and `-light.png`.
