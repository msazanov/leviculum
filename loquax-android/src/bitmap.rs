//! The app's Bitmap as the map's canvas (1155): the frame is composed
//! straight into the Bitmap's pixels, locked through the NDK's
//! jnigraphics (`AndroidBitmap_lockPixels`, in every Android since API 8,
//! no AndroidX), so a frame crosses JNI as its small description only.
//! Before, each frame went into a fresh Java `byte[]` of the frame's size
//! and from there into the Bitmap: two copies and some 20 MB of garbage a
//! frame, about 5 ms a frame outside Rust in Waydroid (the render study
//! of 2026-10-30).
//!
//! Java hands over a Bitmap it does not show: the spare of its three
//! (`MapView`), so the RenderThread never reads pixels being written. The
//! lock is taken when the frame's size is known and released when the
//! [`Locked`] goes, after the frame is drawn; unlocking tells Android the
//! pixels changed, so the next draw uploads them.

#![allow(unsafe_code)]

use std::ffi::c_void;

use jni::objects::JObject;
use jni::sys::{jobject, JNIEnv};

use crate::map::Canvas;

/// `AndroidBitmapInfo` of `<android/bitmap.h>`.
#[repr(C)]
#[derive(Default)]
struct AndroidBitmapInfo {
    width: u32,
    height: u32,
    stride: u32,
    format: i32,
    flags: u32,
}

/// `ANDROID_BITMAP_FORMAT_RGBA_8888`: bytes red, green, blue, alpha, the
/// order the map composes in. `Bitmap.Config.ARGB_8888` is this.
const FORMAT_RGBA_8888: i32 = 1;
/// `ANDROID_BITMAP_RESULT_SUCCESS`.
const SUCCESS: i32 = 0;

#[link(name = "jnigraphics")]
unsafe extern "C" {
    fn AndroidBitmap_getInfo(
        env: *mut JNIEnv,
        bitmap: jobject,
        info: *mut AndroidBitmapInfo,
    ) -> i32;
    fn AndroidBitmap_lockPixels(env: *mut JNIEnv, bitmap: jobject, pixels: *mut *mut c_void)
        -> i32;
    fn AndroidBitmap_unlockPixels(env: *mut JNIEnv, bitmap: jobject) -> i32;
}

/// A Java Bitmap, its pixels locked once a frame asks for them and
/// unlocked when this goes. Lives within one native call: the env and
/// the local reference are that call's.
pub struct Locked {
    env: *mut JNIEnv,
    bitmap: jobject,
    locked: bool,
}

impl Locked {
    /// The Bitmap `bitmap` of the call `env` runs; null for none.
    pub fn new(env: &jni::Env<'_>, bitmap: &JObject<'_>) -> Self {
        Self {
            env: env.get_raw(),
            bitmap: bitmap.as_raw(),
            locked: false,
        }
    }
}

impl Canvas for Locked {
    fn pixels(&mut self, w: u32, h: u32) -> Result<&mut [u8], String> {
        if self.bitmap.is_null() {
            return Err("map: no bitmap to draw into".to_string());
        }
        if self.locked {
            return Err("map: the bitmap is drawn into already".to_string());
        }
        let mut info = AndroidBitmapInfo::default();
        // SAFETY: `env` and `bitmap` are the running native call's, alive
        // until it returns, which outlives `self`; `info` is a valid out
        // pointer of the C layout.
        let got = unsafe { AndroidBitmap_getInfo(self.env, self.bitmap, &mut info) };
        if got != SUCCESS {
            return Err(format!("map: the bitmap's size did not read ({got})"));
        }
        if (info.width, info.height) != (w, h) {
            return Err(format!(
                "map: the bitmap is {}x{}, the frame {w}x{h}",
                info.width, info.height
            ));
        }
        if info.format != FORMAT_RGBA_8888 || info.stride != w.saturating_mul(4) {
            return Err(format!(
                "map: the bitmap is format {} with rows of {} bytes, not RGBA rows of {}",
                info.format,
                info.stride,
                w.saturating_mul(4)
            ));
        }
        let mut pixels: *mut c_void = std::ptr::null_mut();
        // SAFETY: as above; `pixels` is a valid out pointer.
        let got = unsafe { AndroidBitmap_lockPixels(self.env, self.bitmap, &mut pixels) };
        if got != SUCCESS || pixels.is_null() {
            return Err(format!("map: the bitmap did not lock ({got})"));
        }
        self.locked = true;
        let len = h as usize * w as usize * 4;
        // SAFETY: the lock pins `h` rows of `stride` = `w * 4` bytes at
        // `pixels` until `AndroidBitmap_unlockPixels` in `drop`; the
        // slice borrows `self`, so it cannot outlive the lock, and Java
        // does not touch the spare Bitmap while the call runs.
        Ok(unsafe { std::slice::from_raw_parts_mut(pixels.cast::<u8>(), len) })
    }
}

impl Drop for Locked {
    fn drop(&mut self) {
        if self.locked {
            // SAFETY: locked above in this native call, which still runs.
            let _ = unsafe { AndroidBitmap_unlockPixels(self.env, self.bitmap) };
        }
    }
}
