//! Builds the vendored libprojectM with cmake and tells cargo how to link it.
//!
//! Everything is static: one .a per library, merged into the rox binary, so
//! there's no shared object to ship and no runtime search path to get wrong.
//! GL is not linked here on Linux or macOS, because projectM master resolves
//! every GL entry point through its vendored glad using the load proc we hand
//! it at instance creation. Windows is the exception, where the loader itself
//! needs opengl32.

use std::env;
use std::path::{Path, PathBuf};

fn main() {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../vendor/projectm");
    if !source.join("CMakeLists.txt").exists() {
        panic!(
            "vendored libprojectM is missing at {}. Run ./scripts/vendor-projectm.sh from the \
             repo root first; the nix dev shell runs it for you on entry.",
            source.display()
        );
    }

    // The stamp changes when the pinned commits change, which is the only
    // thing that makes the cmake build stale. Watching the tree itself would
    // mean walking several thousand files on every cargo invocation.
    println!(
        "cargo:rerun-if-changed={}",
        source.join(".rox-stamp").display()
    );

    let dst = cmake::Config::new(&source)
        .define("BUILD_SHARED_LIBS", "OFF")
        // The playlist library and the SDL test UI are the two optional
        // pieces; we pick presets ourselves and never open a window here.
        .define("ENABLE_PLAYLIST", "OFF")
        .define("ENABLE_SDL_UI", "OFF")
        .define("BUILD_TESTING", "OFF")
        // Both default to looking for a system copy. There isn't one on any
        // of our three platforms, and vendoring keeps vcpkg out of the
        // Windows build.
        .define("ENABLE_SYSTEM_GLM", "OFF")
        .define("ENABLE_SYSTEM_PROJECTM_EVAL", "OFF")
        .define("ENABLE_GLES", "OFF")
        // Release regardless of the cargo profile. The workspace already
        // builds dependencies optimised (see the profile block in the root
        // Cargo.toml), and a debug projectM misses frames at 60fps. It also
        // keeps ENABLE_DEBUG_POSTFIX from renaming the libraries to *d.
        .profile("Release")
        .build();

    // CMake's GNUInstallDirs picks lib64 on some Linux distributions and lib
    // everywhere else, and which one it picked isn't visible from here.
    println!("cargo:rustc-link-search=native={}/lib", dst.display());
    println!("cargo:rustc-link-search=native={}/lib64", dst.display());

    // One library, not two: the static build merges projectm-eval and the
    // rest of the vendored objects into libprojectM-4.a (checked on this
    // machine, where the install landed in lib64 and carries the prjm_eval_*
    // symbols; the generated projectM-4.pc lists -lprojectM-4 alone). On
    // Windows the static prefix is forced to "lib" by the top-level
    // CMakeLists, so the file is libprojectM-4.lib and static= resolves it
    // from the same link name.
    println!("cargo:rustc-link-lib=static=projectM-4");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    match target_os.as_str() {
        // projectM is C++, and rustc links neither standard library for us.
        "macos" | "ios" => println!("cargo:rustc-link-lib=dylib=c++"),
        "windows" if target_env == "msvc" => {
            // MSVC's runtime comes in through the linker defaults. The GL
            // loader is what needs naming.
            println!("cargo:rustc-link-lib=dylib=opengl32");
        }
        _ => println!("cargo:rustc-link-lib=dylib=stdc++"),
    }

    // Tell dependents where the headers are, in case anything downstream
    // wants to check a constant against them.
    println!(
        "cargo:include={}",
        Path::new(&dst).join("include").display()
    );
    println!("cargo:root={}", dst.display());
}
