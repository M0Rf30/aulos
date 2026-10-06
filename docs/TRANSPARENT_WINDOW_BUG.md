# Transparent/Empty Second Window Appears Alongside Main Application Window

**Status**: Reported to upstream (https://github.com/pop-os/libcosmic/issues/1125)  
**Affects**: Aulos music player on COSMIC Desktop Environment  
**Suspected Component**: libcosmic/iced-wgpu or cosmic-comp  

## Description
When running Aulos (a libcosmic-based music player application), a second transparent/empty window appears alongside the main application window. This window appears to be a winit/wgpu surface but has no visible content and appears transparent.

## Environment
- **OS**: Linux 6.18.9-zen1-2-zen (Arch-based)
- **Desktop**: COSMIC Desktop Environment
- **Compositor**: cosmic-comp (COSMIC Wayland compositor)
- **libcosmic version**: `v1.0.0` (git rev `a3cf875`)
- **iced version**: `v0.14.0-dev` (from libcosmic)
- **wgpu version**: `v22.1.0`
- **winit version**: `v0.30.5` (pop-os fork, tag `iced-xdg-surface-0.13-rc`)
- **Graphics Backend**: wgpu with EGL backend

## Steps to Reproduce
1. Build and run Aulos: `cargo build --release && ./target/release/aulos`
2. Observe two windows appear:
   - Main window with Aulos UI (normal, functional)
   - Second window that is transparent/empty (unexpected)

## Expected Behavior
Only one window should appear - the main application window with Aulos's UI.

## Actual Behavior
Two windows appear. The second window:
- Has no visible content
- Appears transparent
- Seems to be a winit/wgpu surface
- Is associated with the same process (same PID)
- Cannot be closed independently (closes when main window closes)

## Technical Details

### EGL Surface Creation Logs
When running with `RUST_LOG=trace`, extensive EGL surface creation is logged:

```
EGL_MESA_platform_surfaceless
EGL_EXT_surface_compression
EGL_KHR_surfaceless_context
DEBUG wgpu_hal::gles::egl: EGL surface: +srgb
TRACE wgpu_hal::gles::egl: CONFORMANT=0x4D, RENDERABLE=0x4D, NATIVE_RENDERABLE=0x1, SURFACE_TYPE=0x5, ALPHA_SIZE=2
TRACE wgpu_hal::gles::egl: CONFORMANT=0x4D, RENDERABLE=0x4D, NATIVE_RENDERABLE=0x1, SURFACE_TYPE=0x5, ALPHA_SIZE=8
[... many more similar lines with varying ALPHA_SIZE values: 0, 2, 8 ...]
```

Key observations:
- `EGL_MESA_platform_surfaceless` is present
- Multiple surface configurations with different `ALPHA_SIZE` values
- `SURFACE_TYPE=0x5` consistently (window + pbuffer capable)

### Application Architecture
- **Framework**: libcosmic v1.0.0 with `cosmic::Application` trait
- **Single window creation**: Only one `cosmic::app::run()` call in `main.rs`
- **No explicit additional windows**: No dialog, popup, or secondary window creation in application code
- **Context drawer**: Uses standard cosmic context drawer (About, Equalizer, Providers pages)
- **No custom renderers**: No shader widgets or custom wgpu code (visualizer feature is behind `#[cfg(feature = "visualizer")]` and not compiled)

### Process Information
```bash
$ pgrep -fa aulos
542373 /usr/bin/aulos

$ ls -la /proc/542373/fd/ | grep socket | wc -l
14
```
Multiple socket connections exist, suggesting multiple Wayland surfaces are being created.

## Code References

**main.rs** (complete):
```rust
fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let requested_languages = i18n_embed::DesktopLanguageRequester::requested_languages();
    aulos::i18n::init(&requested_languages);

    let settings = cosmic::app::Settings::default()
        .size_limits(
            cosmic::iced::Limits::NONE
                .min_width(900.0)
                .min_height(600.0),
        );

    cosmic::app::run::<aulos::app::AppModel>(settings, ())
}
```

**app.rs** - Application trait implementation (simplified):
```rust
impl cosmic::Application for AppModel {
    const APP_ID: &'static str = "io.github.m0rf30.Aulos";
    
    fn init(core: cosmic::Core, _flags: Self::Flags) -> (Self, Task<cosmic::Action<Self::Message>>) {
        // Standard initialization
        // No window creation, just model setup
        (app, Task::none())
    }
    
    fn view(&self) -> Element<'_, Self::Message> {
        // Single view rendering with nav_bar and content
    }
    
    fn context_drawer(&self) -> Option<context_drawer::ContextDrawer<'_, Self::Message>> {
        // Standard context drawer with About/Equalizer/Providers
        // Only shown when self.core.window.show_context == true
    }
    
    fn subscription(&self) -> Subscription<Self::Message> {
        // Various subscriptions (playback ticker, MPD events, etc.)
        // No window or surface creation
    }
}
```

## Investigation Results

1. **Not application-specific**: The issue persists across different commits, including minimal versions without recent features
2. **EGL/wgpu surface creation**: Logs show extensive EGL surface configuration
3. **No explicit window creation**: Application only calls `cosmic::app::run()` once
4. **Wayland/winit behavior**: Multiple socket connections suggest multiple Wayland surfaces
5. **Unrelated to keyboard shortcuts**: Issue exists before and after keyboard event handler additions

## Possible Causes

1. **wgpu subsurface creation**: wgpu might be creating an additional surface for some internal rendering operation (e.g., staging buffer, compute surface)
2. **COSMIC compositor bug**: The compositor might be incorrectly exposing an internal subsurface as a separate window
3. **libcosmic/iced-wgpu issue**: The graphics backend might be creating an extra surface during initialization or for context drawer rendering
4. **winit xdg-surface handling**: The custom winit fork might have a bug in how it creates/manages XDG surfaces on Wayland

## Workarounds
None found yet. The transparent window is functionally harmless but visually confusing for users.

## Impact
- **Severity**: Low (cosmetic issue)
- **User experience**: Confusing but not blocking
- **Functionality**: No functional impact on application

## Additional Context

This issue was discovered while implementing keyboard shortcuts for the application. Investigation confirmed the transparent window issue is **unrelated to the keyboard implementation** and exists independently in the base application.

## Questions for Maintainers

1. Is this a known issue with libcosmic/iced-wgpu on COSMIC compositor?
2. Are there any wgpu settings or configurations to prevent this extra surface creation?
3. Should libcosmic applications configure the Application trait differently?
4. Is this related to the context drawer implementation?

## Related Links

- **libcosmic repository**: https://github.com/pop-os/libcosmic
- **COSMIC compositor**: https://github.com/pop-os/cosmic-comp
- **winit (pop-os fork)**: https://github.com/pop-os/winit
- **Aulos repository**: https://github.com/M0Rf30/rust-music-player

## Dependency Tree (Relevant Parts)

```
aulos v0.1.0
├── libcosmic v1.0.0 (git: a3cf875)
│   ├── iced v0.14.0-dev
│   │   ├── iced_renderer v0.14.0-dev
│   │   │   ├── iced_wgpu v0.14.0-dev
│   │   │   │   └── wgpu v22.1.0
│   │   │   │       ├── wgpu-core v22.1.0
│   │   │   │       │   ├── wgpu-hal v22.0.0
│   │   ├── iced_winit v0.14.0-dev
│   │   │   └── winit v0.30.5 (pop-os fork)
```

## Next Steps

1. Test with other libcosmic applications to determine if this is Aulos-specific or framework-wide
2. Test on X11 (if available) to see if this is Wayland-specific
3. Try different wgpu backend configurations
4. Report to libcosmic issue tracker if confirmed as framework issue

---

**Report Date**: 2026-02-16  
**Reporter**: Aulos development team  
**File Location**: `docs/TRANSPARENT_WINDOW_BUG.md`
