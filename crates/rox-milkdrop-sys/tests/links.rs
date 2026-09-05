//! Proves the static libraries actually linked. Everything else in projectM's
//! API wants a GL context, and CI has none, so the version string is the one
//! call we can make here: it reads compile-time constants and allocates the
//! result with projectM's allocator, which also exercises the free.

use std::ffi::CStr;

#[test]
fn reports_a_version_from_the_linked_library() {
    unsafe {
        let raw = rox_milkdrop_sys::projectm_get_version_string();
        assert!(!raw.is_null(), "projectm_get_version_string returned null");
        let version = CStr::from_ptr(raw).to_string_lossy().into_owned();
        rox_milkdrop_sys::projectm_free_string(raw);
        // The FBO render entry point we depend on is 4.2.0, so a 3.x library
        // picked up from the system would be a silent wrong-link.
        assert!(
            version.starts_with("4."),
            "expected a projectM 4 library, got {version}"
        );
    }
}
