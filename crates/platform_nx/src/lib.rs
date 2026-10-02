//! Horizon platform layer. Linked by `launcher` on `target_os = "horizon"` only.
#![cfg(target_os = "horizon")]

use core::ffi::{c_char, c_int, c_long, c_void};

mod input;
pub mod swkbd;

unsafe extern "C" {
    fn randomGet(buf: *mut c_void, len: usize);
}

// newlib declares sysconf but libnx does not implement it. std asks for the
// page size and the CPU count; the application gets three of the four cores.
#[unsafe(no_mangle)]
extern "C" fn sysconf(name: c_int) -> c_long {
    const SC_PAGESIZE: c_int = 8;
    const SC_NPROCESSORS_ONLN: c_int = 10;
    match name {
        SC_PAGESIZE => 0x1000,
        SC_NPROCESSORS_ONLN => 3,
        _ => -1,
    }
}

// Static EGL/Mesa: no dynamic loader. Callers (wgpu-hal) treat null as "absent".
#[unsafe(no_mangle)]
extern "C" fn dlopen(_file: *const c_char, _mode: c_int) -> *mut c_void {
    core::ptr::null_mut()
}

#[unsafe(no_mangle)]
extern "C" fn dlsym(_handle: *mut c_void, _name: *const c_char) -> *mut c_void {
    core::ptr::null_mut()
}

#[unsafe(no_mangle)]
extern "C" fn dlclose(_handle: *mut c_void) -> c_int {
    0
}

#[unsafe(no_mangle)]
extern "C" fn dlerror() -> *const c_char {
    c"dlopen unsupported on Horizon".as_ptr()
}

#[unsafe(no_mangle)]
extern "C" fn getrandom(buf: *mut c_void, len: usize, _flags: u32) -> isize {
    unsafe { randomGet(buf, len) };
    len as isize
}

// getrandom 0.4 `custom` backend (`--cfg getrandom_backend="custom"`).
#[unsafe(no_mangle)]
unsafe extern "Rust" fn __getrandom_v03_custom(
    dest: *mut u8,
    len: usize,
) -> Result<(), getrandom::Error> {
    unsafe { randomGet(dest.cast(), len) };
    Ok(())
}

/// Plain-file boot trace at `sdmc:/switch/iw4l/boot.txt`. Works before diag's
/// log exists and from a panic hook; Switch stdio reaches nothing.
// ponytail: unbuffered open-per-line trace; drop once boot is stable.
pub fn breadcrumb(line: &str) {
    use std::io::Write as _;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("sdmc:/switch/iw4l/boot.txt")
    {
        let _ = writeln!(file, "{line}");
    }
}

mod window {
    use core::{ffi::c_void, ptr::NonNull};

    use bevy::{
        app::{AppExit, PluginsState},
        prelude::*,
        window::{
            PrimaryWindow, RawHandleWrapper, RawHandleWrapperHolder, WindowFocused, WindowWrapper,
        },
    };
    use raw_window_handle::{
        AndroidDisplayHandle, AndroidNdkWindowHandle, DisplayHandle, HandleError,
        HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle, WindowHandle,
    };

    unsafe extern "C" {
        fn nwindowGetDefault() -> *mut c_void;
        fn appletMainLoop() -> bool;
        fn appletGetFocusState() -> u8;
    }

    /// libnx's default `NWindow`. The wgpu fork reads an `AndroidNdk` handle
    /// on Horizon as an `NWindow*` for `eglCreateWindowSurface`.
    struct NxWindow(NonNull<c_void>);

    // SAFETY: the default NWindow is a process-lifetime static; wgpu only hands
    // the pointer to EGL.
    unsafe impl Send for NxWindow {}
    unsafe impl Sync for NxWindow {}

    impl HasWindowHandle for NxWindow {
        fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
            let raw = RawWindowHandle::AndroidNdk(AndroidNdkWindowHandle::new(self.0));
            // SAFETY: the pointer outlives `self` (see above).
            Ok(unsafe { WindowHandle::borrow_raw(raw) })
        }
    }

    impl HasDisplayHandle for NxWindow {
        fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
            let raw = RawDisplayHandle::Android(AndroidDisplayHandle::new());
            // SAFETY: an Android display handle carries no pointer.
            Ok(unsafe { DisplayHandle::borrow_raw(raw) })
        }
    }

    /// Replaces winit on Horizon: binds the primary window to the default
    /// `NWindow` and drives `App::update` from `appletMainLoop`.
    /// Must be added after `WindowPlugin` and before the render plugin, which
    /// reads the handle while it builds.
    pub struct NxPlugin;

    impl Plugin for NxPlugin {
        fn build(&self, app: &mut App) {
            crate::breadcrumb("NxPlugin build");
            let window = NonNull::new(unsafe { nwindowGetDefault() }).expect("libnx default NWindow");
            let handle = RawHandleWrapper::new(&WindowWrapper::new(NxWindow(window)))
                .expect("NWindow raw handle");
            let world = app.world_mut();
            let (entity, holder) = world
                .query_filtered::<(Entity, &RawHandleWrapperHolder), With<PrimaryWindow>>()
                .single(world)
                .map(|(entity, holder)| (entity, holder.clone()))
                .expect("NxPlugin needs WindowPlugin's primary window");
            *holder.0.lock().expect("window handle lock") = Some(handle.clone());
            world.entity_mut(entity).insert(handle);
            app.add_plugins(crate::input::InputPlugin).set_runner(run);
            crate::breadcrumb("NxPlugin ready");
        }
    }

    /// Mirrors winit's focus handling: the game reads `Window::focused`.
    fn set_focus(app: &mut App, focused: bool) {
        let world = app.world_mut();
        let Ok(entity) = world
            .query_filtered::<Entity, With<PrimaryWindow>>()
            .single(world)
        else {
            return;
        };
        if let Some(mut window) = world.get_mut::<Window>(entity) {
            window.focused = focused;
        }
        world.write_message(WindowFocused { window: entity, focused });
    }

    fn run(mut app: App) -> AppExit {
        if app.plugins_state() != PluginsState::Cleaned {
            while app.plugins_state() == PluginsState::Adding {
                bevy::tasks::tick_global_task_pools_on_main_thread();
            }
            app.finish();
            app.cleanup();
        }
        crate::breadcrumb("runner: plugins finished");
        let mut frames = 0u64;
        let mut focused = true;
        // appletMainLoop turns false when the user closes the app from HOME.
        while unsafe { appletMainLoop() } {
            // AppletFocusState_InFocus = 1; HOME menu, sleep mode and system
            // applets (software keyboard) all take the focus away.
            let now_focused = unsafe { appletGetFocusState() } == 1;
            if now_focused != focused {
                focused = now_focused;
                set_focus(&mut app, focused);
                crate::breadcrumb(&format!("runner: focus {focused}"));
            } else if !focused {
                // Nothing to draw to while backgrounded; idle until HOME returns.
                std::thread::sleep(std::time::Duration::from_millis(16));
                continue;
            }
            app.update();
            frames += 1;
            if frames == 1 {
                crate::breadcrumb("runner: first update done");
            }
            if let Some(exit) = app.should_exit() {
                return exit;
            }
        }
        AppExit::Success
    }
}

pub use window::NxPlugin;
