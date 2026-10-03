//! Horizon platform layer. Linked by `launcher` on `target_os = "horizon"` only.
#![cfg(target_os = "horizon")]

use core::ffi::{c_char, c_int, c_long, c_void};

mod input;
pub mod swkbd;

unsafe extern "C" {
    fn randomGet(buf: *mut c_void, len: usize);
    fn socketInitialize(config: *const c_void) -> u32;
    fn nxlinkConnectToHost(redir_stdout: bool, redir_stderr: bool) -> c_int;
    static __nxlink_host: u32;
    fn svcGetInfo(out: *mut u64, id0: u32, handle: u32, id1: u64) -> u32;
    fn appletGetAppletType() -> u32;
    fn __libnx_exit(code: c_int) -> !;
    static __code_start: u8;
    static __rodata_start: u8;
}

// libnx calls this before main. Only an `nxlink -s` launch needs the BSD
// driver (stdout/stderr -> host); the socket buffers come out of a tight heap,
// and nothing else on Horizon opens sockets.
#[unsafe(no_mangle)]
extern "C" fn userAppInit() {
    // SAFETY: plain FFI; __nxlink_host is set by the loader before this runs.
    unsafe {
        if __nxlink_host != 0 && socketInitialize(core::ptr::null()) == 0 {
            nxlinkConnectToHost(true, true);
        }
    }
}

static IN_EXIT: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

// Native abort() (Mesa, libdrm_nouveau, libstdc++) otherwise returns to hbmenu
// silently. Log the caller; resolve with
//   addr2line -f -e iw4l.elf $(printf %x $((0x<link abort> + <ret> - <abort>)))
// where <link abort> is `aarch64-none-elf-nm iw4l.elf | grep ' T abort'`.
#[unsafe(no_mangle)]
extern "C" fn abort() -> ! {
    let lr: usize;
    // SAFETY: reads the link register before any call clobbers it.
    unsafe { core::arch::asm!("mov {}, x30", out(reg) lr) };
    if !IN_EXIT.swap(true, core::sync::atomic::Ordering::Relaxed) {
        breadcrumb(&format!(
            "abort: ret={lr:#x} abort={:#x} {} {}",
            abort as usize,
            mem_line(),
            stack_trace()
        ));
    }
    unsafe { __libnx_exit(134) }
}

// Replaces libsysbase's `_exit` (same libnx exit) so every exit leaves a line.
#[unsafe(no_mangle)]
extern "C" fn _exit(code: c_int) -> ! {
    if !IN_EXIT.swap(true, core::sync::atomic::Ordering::Relaxed) {
        breadcrumb(&format!("_exit: code={code} {}", mem_line()));
    }
    unsafe { __libnx_exit(code) }
}

/// Code addresses in the next 2 KiB of stack, as offsets from the load base:
/// `addr2line -f -e iw4l.elf <offset>`. Release builds keep no frame pointers,
/// so this is a conservative scan, not an unwind: expect some stale entries.
// ponytail: boot-debug only, drop once boot is stable.
fn stack_trace() -> String {
    let sp: usize;
    // SAFETY: reads the stack pointer.
    unsafe { core::arch::asm!("mov {}, sp", out(reg) sp) };
    // SAFETY: linker-provided section markers; only their addresses are used.
    let (lo, hi) = unsafe {
        (
            core::ptr::addr_of!(__code_start) as usize,
            core::ptr::addr_of!(__rodata_start) as usize,
        )
    };
    let mut out = format!("base={lo:#x} bt=");
    let mut found = 0;
    for i in 0..256 {
        // SAFETY: the frames above the current sp are mapped stack.
        let word = unsafe { core::ptr::read_volatile((sp + i * 8) as *const usize) };
        if (lo..hi).contains(&word) && word % 4 == 0 {
            out.push_str(&format!("{:x} ", word - lo));
            found += 1;
            if found == 24 {
                break;
            }
        }
    }
    out
}

// libnx hands out 12 pthread keys per process (`threadTlsAlloc`); std, tracing,
// SDL, Mesa and bevy want more, and std aborts with "out of TLS keys". The
// `--wrap=pthread_key_*` hooks (launcher/build.rs) serve unlimited keys from
// one real libnx key whose per-thread value is a heap table of KEYS slots.
mod tls {
    use core::{
        ffi::{c_int, c_uint, c_void},
        sync::atomic::{AtomicU8, AtomicUsize, Ordering::*},
    };

    const KEYS: usize = 512;
    type Dtor = Option<unsafe extern "C" fn(*mut c_void)>;

    unsafe extern "C" {
        fn __real_pthread_key_create(key: *mut c_uint, dtor: Dtor) -> c_int;
        fn __real_pthread_setspecific(key: c_uint, value: *const c_void) -> c_int;
        fn __real_pthread_getspecific(key: c_uint) -> *mut c_void;
        fn calloc(n: usize, size: usize) -> *mut c_void;
        fn free(ptr: *mut c_void);
    }

    // 0 free, 1 taken; DTORS[i] is the key's destructor (0 = none).
    static USED: [AtomicU8; KEYS] = [const { AtomicU8::new(0) }; KEYS];
    static DTORS: [AtomicUsize; KEYS] = [const { AtomicUsize::new(0) }; KEYS];
    // 0 = not made, 1 = being made, 2 = ready.
    static STATE: AtomicU8 = AtomicU8::new(0);
    static REAL: AtomicUsize = AtomicUsize::new(0);

    pub fn in_use() -> usize {
        USED.iter().filter(|u| u.load(Relaxed) != 0).count()
    }

    fn real_key() -> c_uint {
        if STATE.compare_exchange(0, 1, AcqRel, Acquire).is_ok() {
            let mut key = 0;
            // SAFETY: plain FFI into an out-parameter.
            let rc = unsafe { __real_pthread_key_create(&mut key, Some(thread_exit)) };
            assert_eq!(rc, 0, "no libnx TLS slot left for the key table");
            REAL.store(key as usize, Relaxed);
            STATE.store(2, Release);
        }
        while STATE.load(Acquire) != 2 {
            core::hint::spin_loop();
        }
        REAL.load(Relaxed) as c_uint
    }

    /// This thread's value table; allocated on first use when `make`.
    fn table(make: bool) -> *mut *mut c_void {
        let real = real_key();
        // SAFETY: plain FFI on our own key.
        let mut t = unsafe { __real_pthread_getspecific(real) }.cast::<*mut c_void>();
        if t.is_null() && make {
            // SAFETY: calloc'd table, owned by this thread until `thread_exit`.
            t = unsafe { calloc(KEYS, size_of::<*mut c_void>()) }.cast();
            if !t.is_null() {
                unsafe { __real_pthread_setspecific(real, t.cast()) };
            }
        }
        t
    }

    // POSIX order: up to four rounds, since a destructor may set values again.
    unsafe extern "C" fn thread_exit(table: *mut c_void) {
        let t = table.cast::<*mut c_void>();
        for _ in 0..4 {
            for i in 0..KEYS {
                // SAFETY: `t` has KEYS entries and belongs to the exiting thread.
                let value = unsafe { t.add(i).replace(core::ptr::null_mut()) };
                let dtor = DTORS[i].load(Relaxed);
                if !value.is_null() && dtor != 0 {
                    // SAFETY: stored from a `Dtor` in `create`.
                    unsafe { core::mem::transmute::<usize, unsafe extern "C" fn(*mut c_void)>(dtor)(value) };
                }
            }
        }
        // SAFETY: allocated with calloc in `table`.
        unsafe { free(table) };
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn __wrap_pthread_key_create(key: *mut c_uint, dtor: Dtor) -> c_int {
        if key.is_null() {
            return 22; // EINVAL
        }
        for i in 0..KEYS {
            if USED[i].compare_exchange(0, 1, AcqRel, Relaxed).is_ok() {
                DTORS[i].store(dtor.map_or(0, |d| d as usize), Relaxed);
                // SAFETY: caller's out-parameter.
                unsafe { *key = i as c_uint };
                return 0;
            }
        }
        11 // EAGAIN, as POSIX says for PTHREAD_KEYS_MAX
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn __wrap_pthread_key_delete(key: c_uint) -> c_int {
        match USED.get(key as usize) {
            Some(u) if u.swap(0, AcqRel) != 0 => {
                DTORS[key as usize].store(0, Relaxed);
                0
            }
            _ => 22,
        }
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn __wrap_pthread_setspecific(key: c_uint, value: *const c_void) -> c_int {
        let t = table(true);
        if t.is_null() {
            return 12; // ENOMEM
        }
        if key as usize >= KEYS {
            return 22;
        }
        // SAFETY: in range of this thread's table.
        unsafe { *t.add(key as usize) = value.cast_mut() };
        0
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn __wrap_pthread_getspecific(key: c_uint) -> *mut c_void {
        let t = table(false);
        if t.is_null() || key as usize >= KEYS {
            return core::ptr::null_mut();
        }
        // SAFETY: in range of this thread's table.
        unsafe { *t.add(key as usize) }
    }
}

/// Process memory in use / available; `svcGetInfo` on the current process.
fn mem_line() -> String {
    const CUR_PROCESS: u32 = 0xFFFF_8001;
    let (mut total, mut used) = (0u64, 0u64);
    // SAFETY: plain FFI into out-parameters.
    unsafe {
        svcGetInfo(&mut total, 6, CUR_PROCESS, 0);
        svcGetInfo(&mut used, 7, CUR_PROCESS, 0);
    }
    format!("mem={}/{}MB tls_keys={}", used >> 20, total >> 20, tls::in_use())
}

/// Launch context: applet type (0 = title takeover) and memory budget.
pub fn boot_env() {
    // SAFETY: plain FFI; applet services are up by `main`.
    let applet = unsafe { appletGetAppletType() };
    breadcrumb(&format!("env: applet_type={applet} {}", mem_line()));
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
        // A kernel panic resets the console before FAT flushes a closed file.
        let _ = file.sync_all();
    }
}

/// `io::Write` into `boot.txt`, for Bevy's log layer: stderr reaches nothing.
pub struct BootLog;

impl std::io::Write for BootLog {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        breadcrumb(String::from_utf8_lossy(buf).trim_end());
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
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

    /// What the GL backend offers on switch-mesa; sizes the bound-slot render path.
    // ponytail: boot-debug only, drop once the render path is settled.
    fn gpu_caps(app: &mut App) {
        use bevy::render::{RenderApp, renderer::RenderAdapter};
        let Some(adapter) = app
            .get_sub_app(RenderApp)
            .and_then(|render| render.world().get_resource::<RenderAdapter>())
        else {
            return;
        };
        let (info, l) = (adapter.get_info(), adapter.limits());
        crate::breadcrumb(&format!(
            "gpu: {} / {} / {:?}\ngpu features: {:?}\ngpu downlevel: {:?}\n\
             gpu limits: sampled_tex/stage={} samplers/stage={} storage_buf/stage={} \
             storage_buf_size={} uniform_buf/stage={} uniform_buf_size={} bind_groups={} \
             tex2d={} immediates={} vertex_attrs={}",
            info.name,
            info.driver_info,
            info.backend,
            adapter.features(),
            adapter.get_downlevel_capabilities().flags,
            l.max_sampled_textures_per_shader_stage,
            l.max_samplers_per_shader_stage,
            l.max_storage_buffers_per_shader_stage,
            l.max_storage_buffer_binding_size,
            l.max_uniform_buffers_per_shader_stage,
            l.max_uniform_buffer_binding_size,
            l.max_bind_groups,
            l.max_texture_dimension_2d,
            l.max_immediate_size,
            l.max_vertex_attributes,
        ));
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
        gpu_caps(&mut app);
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
