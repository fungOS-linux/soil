//! JNI bridge: run the Soil Canvas guest and hand RGBA frames to the Canvas
//! Android view. Platform pixels stay in the host; the guest only calls the
//! granted `soil:canvas` surface.
//!
//! All native calls for one session are expected from the same Java executor
//! thread. The Java view enforces that so the wasmtime store is never used
//! concurrently.

use std::panic::{AssertUnwindSafe, catch_unwind};

use anyhow::{Context, Result};
use jni::JNIEnv;
use jni::objects::{JClass, JIntArray};
use jni::sys::{jint, jintArray, jlong};
use soil_runtime::{
    DEFAULT_FUEL, GuestScene, Key,
    canvas::{CANVAS_GUEST, OwnerSurface, SurfaceSize},
};

const INNER_WIDTH: u32 = 480;
const INNER_HEIGHT: u32 = 640;

struct Session {
    guest: GuestScene,
    width: u32,
    height: u32,
}

fn key_from_java(value: jint) -> Option<Key> {
    Some(match value {
        1 => Key::Up,
        2 => Key::Down,
        3 => Key::Left,
        4 => Key::Right,
        5 => Key::Select,
        6 => Key::Back,
        7 => Key::Home,
        _ => return None,
    })
}

fn create_session(width: jint, height: jint) -> Result<Box<Session>> {
    let (width, height, size) = if (1..=160).contains(&width) && (1..=160).contains(&height) {
        (128, 128, SurfaceSize::Outer)
    } else {
        (INNER_WIDTH, INNER_HEIGHT, SurfaceSize::Inner)
    };
    let surface = OwnerSurface::with_size(size).context("create Canvas owner surface")?;
    let guest = GuestScene::with_canvas(CANVAS_GUEST, DEFAULT_FUEL, Some(Box::new(surface)))
        .context("instantiate Soil Canvas guest")?;
    Ok(Box::new(Session {
        guest,
        width,
        height,
    }))
}

fn throw(env: &mut JNIEnv<'_>, message: impl Into<String>) {
    let _ = env.throw_new("java/lang/IllegalStateException", message.into());
}

/// Java: `SoilSurface.nativeCreate(width, height) -> long handle`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_fpl_canvas_SoilSurface_nativeCreate(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    width: jint,
    height: jint,
) -> jlong {
    match catch_unwind(AssertUnwindSafe(|| create_session(width, height))) {
        Ok(Ok(session)) => Box::into_raw(session) as jlong,
        Ok(Err(error)) => {
            throw(&mut env, format!("Soil Canvas init failed: {error:#}"));
            0
        }
        Err(_) => {
            throw(&mut env, "Soil Canvas init panicked");
            0
        }
    }
}

/// Java: `SoilSurface.nativePresent(handle) -> int[] ARGB`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_fpl_canvas_SoilSurface_nativePresent(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
) -> jintArray {
    if handle == 0 {
        return std::ptr::null_mut();
    }
    // SAFETY: Java owns the handle and serializes calls onto one executor.
    let session = unsafe { &mut *(handle as *mut Session) };
    let rendered = catch_unwind(AssertUnwindSafe(|| session.guest.render_canvas()));
    let frame = match rendered {
        Ok(Ok(frame)) => frame,
        Ok(Err(error)) => {
            throw(&mut env, format!("Soil Canvas present failed: {error:#}"));
            return std::ptr::null_mut();
        }
        Err(_) => {
            throw(&mut env, "Soil Canvas present panicked");
            return std::ptr::null_mut();
        }
    };
    if frame.width != session.width
        || frame.height != session.height
        || frame.pixels.len() != (frame.width as usize * frame.height as usize * 4)
    {
        throw(&mut env, "Soil Canvas returned an invalid owner frame");
        return std::ptr::null_mut();
    }
    let mut pixels = Vec::with_capacity(frame.width as usize * frame.height as usize);
    for pixel in frame.pixels.as_chunks::<4>().0 {
        let argb = (u32::from(pixel[3]) << 24)
            | (u32::from(pixel[0]) << 16)
            | (u32::from(pixel[1]) << 8)
            | u32::from(pixel[2]);
        pixels.push(argb as jint);
    }
    let array: JIntArray<'_> = match env.new_int_array(pixels.len() as i32) {
        Ok(array) => array,
        Err(error) => {
            throw(&mut env, format!("Soil Canvas alloc failed: {error}"));
            return std::ptr::null_mut();
        }
    };
    if let Err(error) = env.set_int_array_region(&array, 0, &pixels) {
        throw(&mut env, format!("Soil Canvas copy failed: {error}"));
        return std::ptr::null_mut();
    }
    array.into_raw()
}

/// Java: `SoilSurface.nativeInput(handle, key)`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_fpl_canvas_SoilSurface_nativeInput(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    key: jint,
) {
    if handle == 0 {
        return;
    }
    let Some(key) = key_from_java(key) else {
        return;
    };
    // SAFETY: same contract as nativePresent.
    let session = unsafe { &mut *(handle as *mut Session) };
    if let Err(error) = session.guest.key(key) {
        throw(&mut env, format!("Soil Canvas input failed: {error:#}"));
    }
}

/// Java: `SoilSurface.nativeDestroy(handle)`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_fpl_canvas_SoilSurface_nativeDestroy(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
) {
    if handle == 0 {
        return;
    }
    // SAFETY: Java calls this exactly once, from the serialized executor.
    drop(unsafe { Box::from_raw(handle as *mut Session) });
}

/// Java: `SoilSurface.nativeFrameSize() -> int[] {width, height}`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_fpl_canvas_SoilSurface_nativeFrameSize(
    env: JNIEnv<'_>,
    _class: JClass<'_>,
) -> jintArray {
    let values = [INNER_WIDTH as jint, INNER_HEIGHT as jint];
    match env.new_int_array(values.len() as i32) {
        Ok(array) => {
            if env.set_int_array_region(&array, 0, &values).is_err() {
                return std::ptr::null_mut();
            }
            array.into_raw()
        }
        Err(_) => std::ptr::null_mut(),
    }
}
