//! Raw declarations for the part of libprojectM's C API the Milkdrop panel
//! uses. Nothing here is safe, nothing here is idiomatic, and nothing here
//! knows about rox: the wrapping, the GL context and the worker thread all
//! live a layer up in `rox-milkdrop`.
//!
//! The bindings are hand-written rather than generated. bindgen wants
//! libclang, which nothing else in the toolchain needs and which we'd then
//! owe on three platforms and in the nix build, and the surface below is
//! about twenty functions that have been stable across projectM 4.x. Each
//! signature is transcribed from the headers under
//! `vendor/projectm/src/api/include/projectM-4/`; when a header and a
//! declaration here disagree, the header wins.
//!
//! `bool` in those headers is C's `_Bool`, one byte on every target we build
//! for, so Rust's `bool` is the right ABI match.
//!
//! libprojectM is LGPL-2.1-only and linked statically into a binary whose own
//! source ships under AGPL-3.0-only, which satisfies the relink clause.

// The C API's names are what a reader has to grep for in projectM's headers
// and docs, so they keep their C spelling.
#![allow(non_camel_case_types)]

use core::ffi::{c_char, c_void};

/// Opaque instance pointer. The header types it as `struct projectm*`.
pub type projectm_handle = *mut c_void;

/// GL entry point resolver, handed to projectM at creation so its vendored
/// glad can load through our context instead of linking libGL.
pub type projectm_load_proc =
    Option<unsafe extern "C" fn(name: *const c_char, user_data: *mut c_void) -> *mut c_void>;

/// Layout of the samples passed to [`projectm_pcm_add_float`].
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum projectm_channels {
    PROJECTM_MONO = 1,
    PROJECTM_STEREO = 2,
}

/// Fired when projectM wants a different preset, either on its timer or on a
/// beat when hard cuts are on.
pub type projectm_preset_switch_requested_event =
    Option<unsafe extern "C" fn(is_hard_cut: bool, user_data: *mut c_void)>;

/// Fired when a preset failed to load. Both strings are only valid for the
/// duration of the call.
pub type projectm_preset_switch_failed_event = Option<
    unsafe extern "C" fn(
        preset_filename: *const c_char,
        message: *const c_char,
        user_data: *mut c_void,
    ),
>;

extern "C" {
    // core.h
    pub fn projectm_create_with_opengl_load_proc(
        load_proc: projectm_load_proc,
        user_data: *mut c_void,
    ) -> projectm_handle;
    pub fn projectm_destroy(instance: projectm_handle);
    pub fn projectm_load_preset_file(
        instance: projectm_handle,
        filename: *const c_char,
        smooth_transition: bool,
    );
    pub fn projectm_load_preset_data(
        instance: projectm_handle,
        data: *const c_char,
        smooth_transition: bool,
    );
    /// Caller owns the returned string and frees it with
    /// [`projectm_free_string`].
    pub fn projectm_get_version_string() -> *mut c_char;

    // memory.h
    pub fn projectm_free_string(string: *const c_char);

    // render_opengl.h
    pub fn projectm_opengl_render_frame_fbo(instance: projectm_handle, framebuffer_object_id: u32);

    // parameters.h
    pub fn projectm_set_window_size(instance: projectm_handle, width: usize, height: usize);
    pub fn projectm_set_fps(instance: projectm_handle, fps: i32);
    pub fn projectm_set_beat_sensitivity(instance: projectm_handle, sensitivity: f32);
    pub fn projectm_set_preset_duration(instance: projectm_handle, seconds: f64);
    pub fn projectm_set_hard_cut_enabled(instance: projectm_handle, enabled: bool);
    pub fn projectm_set_preset_locked(instance: projectm_handle, lock: bool);
    pub fn projectm_set_aspect_correction(instance: projectm_handle, enabled: bool);
    pub fn projectm_set_mesh_size(instance: projectm_handle, width: usize, height: usize);
    pub fn projectm_set_texture_search_paths(
        instance: projectm_handle,
        texture_search_paths: *const *const c_char,
        count: usize,
    );

    // audio.h
    pub fn projectm_pcm_get_max_samples() -> u32;
    pub fn projectm_pcm_add_float(
        instance: projectm_handle,
        samples: *const f32,
        count: u32,
        channels: projectm_channels,
    );

    // callbacks.h
    pub fn projectm_set_preset_switch_requested_event_callback(
        instance: projectm_handle,
        callback: projectm_preset_switch_requested_event,
        user_data: *mut c_void,
    );
    pub fn projectm_set_preset_switch_failed_event_callback(
        instance: projectm_handle,
        callback: projectm_preset_switch_failed_event,
        user_data: *mut c_void,
    );
}
