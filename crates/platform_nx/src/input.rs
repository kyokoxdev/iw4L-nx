//! SDL2 GameController -> Bevy gamepad messages, standing in for bevy_gilrs.

use core::ffi::{c_char, c_int, c_void};
use std::collections::HashMap;

use bevy::{
    input::{
        InputSystems,
        gamepad::{
            GamepadAxis, GamepadButton, GamepadConnection, GamepadConnectionEvent,
            GamepadRumbleRequest, RawGamepadAxisChangedEvent, RawGamepadButtonChangedEvent,
            RawGamepadEvent,
        },
    },
    prelude::*,
};

const SDL_INIT_GAMECONTROLLER: u32 = 0x2000;
const SDL_INIT_EVENTS: u32 = 0x4000;
const AXIS_MOTION: u32 = 0x650;
const BUTTON_DOWN: u32 = 0x651;
const BUTTON_UP: u32 = 0x652;
const DEVICE_ADDED: u32 = 0x653;
const DEVICE_REMOVED: u32 = 0x654;

/// `SDL_Event` is a 56-byte union; fields are read at their C offsets.
#[repr(C, align(8))]
struct SdlEvent([u8; 56]);

impl SdlEvent {
    fn kind(&self) -> u32 {
        u32::from_ne_bytes(self.0[0..4].try_into().unwrap())
    }
    /// Device index (added) or instance id (everything else).
    fn which(&self) -> i32 {
        i32::from_ne_bytes(self.0[8..12].try_into().unwrap())
    }
    /// Axis id or button id.
    fn control(&self) -> u8 {
        self.0[12]
    }
    fn axis_value(&self) -> i16 {
        i16::from_ne_bytes(self.0[16..18].try_into().unwrap())
    }
}

unsafe extern "C" {
    fn SDL_SetHint(name: *const c_char, value: *const c_char) -> c_int;
    fn SDL_InitSubSystem(flags: u32) -> c_int;
    fn SDL_PollEvent(event: *mut SdlEvent) -> c_int;
    fn SDL_NumJoysticks() -> c_int;
    fn SDL_IsGameController(index: c_int) -> c_int;
    fn SDL_GameControllerOpen(index: c_int) -> *mut c_void;
    fn SDL_GameControllerClose(pad: *mut c_void);
    fn SDL_GameControllerName(pad: *mut c_void) -> *const c_char;
    fn SDL_GameControllerGetJoystick(pad: *mut c_void) -> *mut c_void;
    fn SDL_JoystickInstanceID(joystick: *mut c_void) -> i32;
    fn SDL_GameControllerRumble(pad: *mut c_void, low: u16, high: u16, ms: u32) -> c_int;
}

struct Pad {
    entity: Entity,
    handle: *mut c_void,
}

/// SDL controller handles are main-thread only; Bevy keeps this off other threads.
#[derive(Default)]
struct Pads(HashMap<i32, Pad>);

pub(crate) struct InputPlugin;

impl Plugin for InputPlugin {
    fn build(&self, app: &mut App) {
        // Positional buttons: SDL "A" is the bottom face button on every pad,
        // matching Bevy's South and the PC bindings.
        // SAFETY: plain FFI with static strings.
        unsafe {
            SDL_SetHint(c"SDL_GAMECONTROLLER_USE_BUTTON_LABELS".as_ptr(), c"0".as_ptr());
            if SDL_InitSubSystem(SDL_INIT_GAMECONTROLLER | SDL_INIT_EVENTS) != 0 {
                crate::breadcrumb("input: SDL controller init failed");
            }
        }
        app.insert_non_send(Pads::default())
            .add_systems(PreUpdate, poll.before(InputSystems))
            .add_systems(PostUpdate, rumble);
    }
}

type Writers<'w> = (
    MessageWriter<'w, RawGamepadEvent>,
    MessageWriter<'w, GamepadConnectionEvent>,
    MessageWriter<'w, RawGamepadButtonChangedEvent>,
    MessageWriter<'w, RawGamepadAxisChangedEvent>,
);

fn poll(
    mut commands: Commands,
    mut pads: NonSendMut<Pads>,
    mut scanned: Local<bool>,
    writers: Writers,
) {
    let (mut raw, mut connections, mut buttons, mut axes) = writers;
    // Controllers present at init may or may not come as DEVICE_ADDED; scan
    // once, `open` ignores ones already known.
    if !std::mem::replace(&mut *scanned, true) {
        // SAFETY: plain FFI.
        for index in 0..unsafe { SDL_NumJoysticks() } {
            if unsafe { SDL_IsGameController(index) } != 0 {
                open(index, &mut commands, &mut pads, &mut raw, &mut connections);
            }
        }
    }
    let mut event = SdlEvent([0; 56]);
    // SAFETY: `event` is a valid, 8-aligned 56-byte buffer.
    while unsafe { SDL_PollEvent(&mut event) } != 0 {
        match event.kind() {
            DEVICE_ADDED => open(event.which(), &mut commands, &mut pads, &mut raw, &mut connections),
            DEVICE_REMOVED => {
                if let Some(pad) = pads.0.remove(&event.which()) {
                    let gone = GamepadConnectionEvent::new(pad.entity, GamepadConnection::Disconnected);
                    raw.write(gone.clone().into());
                    connections.write(gone);
                    // SAFETY: handle came from SDL_GameControllerOpen and is closed once.
                    unsafe { SDL_GameControllerClose(pad.handle) };
                }
            }
            kind @ (BUTTON_DOWN | BUTTON_UP) => {
                let Some(pad) = pads.0.get(&event.which()) else { continue };
                let Some(button) = convert_button(event.control()) else { continue };
                let value = f32::from(kind == BUTTON_DOWN);
                let changed = RawGamepadButtonChangedEvent::new(pad.entity, button, value);
                raw.write(changed.clone().into());
                buttons.write(changed);
            }
            AXIS_MOTION => {
                let Some(pad) = pads.0.get(&event.which()) else { continue };
                let value = f32::from(event.axis_value()) / 32767.0;
                match event.control() {
                    0 => axis(pad.entity, GamepadAxis::LeftStickX, value, &mut raw, &mut axes),
                    1 => axis(pad.entity, GamepadAxis::LeftStickY, -value, &mut raw, &mut axes),
                    2 => axis(pad.entity, GamepadAxis::RightStickX, value, &mut raw, &mut axes),
                    3 => axis(pad.entity, GamepadAxis::RightStickY, -value, &mut raw, &mut axes),
                    trigger @ (4 | 5) => {
                        let button = if trigger == 4 {
                            GamepadButton::LeftTrigger2
                        } else {
                            GamepadButton::RightTrigger2
                        };
                        let changed = RawGamepadButtonChangedEvent::new(
                            pad.entity,
                            button,
                            value.clamp(0.0, 1.0),
                        );
                        raw.write(changed.clone().into());
                        buttons.write(changed);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

fn axis(
    entity: Entity,
    axis: GamepadAxis,
    value: f32,
    raw: &mut MessageWriter<RawGamepadEvent>,
    axes: &mut MessageWriter<RawGamepadAxisChangedEvent>,
) {
    let changed = RawGamepadAxisChangedEvent::new(entity, axis, value.clamp(-1.0, 1.0));
    raw.write(changed.clone().into());
    axes.write(changed);
}

fn open(
    index: i32,
    commands: &mut Commands,
    pads: &mut Pads,
    raw: &mut MessageWriter<RawGamepadEvent>,
    connections: &mut MessageWriter<GamepadConnectionEvent>,
) {
    // SAFETY: plain FFI; the handle is stored and closed on removal.
    let handle = unsafe { SDL_GameControllerOpen(index) };
    if handle.is_null() {
        return;
    }
    // SAFETY: `handle` is a live controller.
    let id = unsafe { SDL_JoystickInstanceID(SDL_GameControllerGetJoystick(handle)) };
    if pads.0.contains_key(&id) {
        // SDL refcounts opens: balance the extra one.
        unsafe { SDL_GameControllerClose(handle) };
        return;
    }
    // SAFETY: SDL returns a NUL-terminated name or null.
    let name = unsafe {
        let name = SDL_GameControllerName(handle);
        if name.is_null() {
            "Switch controller".to_owned()
        } else {
            std::ffi::CStr::from_ptr(name).to_string_lossy().into_owned()
        }
    };
    let entity = commands.spawn_empty().id();
    pads.0.insert(id, Pad { entity, handle });
    let joined = GamepadConnectionEvent::new(
        entity,
        GamepadConnection::Connected { name, vendor_id: None, product_id: None },
    );
    raw.write(joined.clone().into());
    connections.write(joined);
}

fn convert_button(button: u8) -> Option<GamepadButton> {
    Some(match button {
        0 => GamepadButton::South,
        1 => GamepadButton::East,
        2 => GamepadButton::West,
        3 => GamepadButton::North,
        4 => GamepadButton::Select,
        5 => GamepadButton::Mode,
        6 => GamepadButton::Start,
        7 => GamepadButton::LeftThumb,
        8 => GamepadButton::RightThumb,
        9 => GamepadButton::LeftTrigger,
        10 => GamepadButton::RightTrigger,
        11 => GamepadButton::DPadUp,
        12 => GamepadButton::DPadDown,
        13 => GamepadButton::DPadLeft,
        14 => GamepadButton::DPadRight,
        _ => return None,
    })
}

fn rumble(mut requests: MessageReader<GamepadRumbleRequest>, pads: NonSend<Pads>) {
    for request in requests.read() {
        let (gamepad, low, high, ms) = match request {
            GamepadRumbleRequest::Add { gamepad, intensity, duration } => (
                *gamepad,
                (intensity.strong_motor.clamp(0.0, 1.0) * f32::from(u16::MAX)) as u16,
                (intensity.weak_motor.clamp(0.0, 1.0) * f32::from(u16::MAX)) as u16,
                duration.as_millis().min(u128::from(u32::MAX)) as u32,
            ),
            GamepadRumbleRequest::Stop { gamepad } => (*gamepad, 0, 0, 0),
        };
        if let Some(pad) = pads.0.values().find(|pad| pad.entity == gamepad) {
            // SAFETY: handle is open while it is in `pads`. Unsupported rumble is not an error here.
            unsafe { SDL_GameControllerRumble(pad.handle, low, high, ms) };
        }
    }
}
