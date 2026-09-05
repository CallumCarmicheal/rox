#!/usr/bin/env bash
# Fetches the pinned libprojectM source that crates/rox-milkdrop-sys builds
# with cmake, plus its projectm-eval submodule, into vendor/projectm. The nix
# shellHook runs this on shell entry; run it by hand once before building
# without nix. Stamped on the two commit shas, so it's a no-op when nothing
# moved. Unlike vendor-gpui.sh there are no patches here: we build projectM
# as it ships.
set -euo pipefail
cd "$(dirname "$0")/.."

# Same collation pin as vendor-gpui.sh, for the tar and find calls below.
export LC_ALL=C

# projectM master, not the 4.1.7 release: RenderFrame in 4.1.7 rebinds the
# default framebuffer at the end, which a surfaceless context doesn't have.
# Master has projectm_opengl_render_frame_fbo and
# projectm_create_with_opengl_load_proc, both @since 4.2.0.
# repo commit sha256-of-the-codeload-tarball
projectm_commit="88f23c76743a38c6d8456a8c354c62186270f661"
projectm_sha256="4b6bf022dafbb8eba86bb150bd46e7fabc175ab2cc9279e521b30dd980f1ec70"
# The vendor/projectm-eval submodule of the commit above, per its .gitmodules.
eval_commit="22fb0cfd8f2dfbcd2b68f2443e7f44e19b32c09a"
eval_sha256="48002253353392393a1a5a4d2b0fc04497cfff774f3a9df084ef5078aa4f28ee"

out="vendor/projectm"
stamp="$out/.rox-stamp"
want="$projectm_commit-$eval_commit"

if [[ -f $stamp && $(<"$stamp") == "$want" ]]; then
    exit 0
fi

checksum() {
    if command -v sha256sum >/dev/null; then
        sha256sum
    else
        shasum -a 256
    fi | cut -d' ' -f1
}

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

fetch() {
    local repo=$1 commit=$2 sha256=$3 dest=$4
    curl -fsSL "https://codeload.github.com/projectM-visualizer/$repo/tar.gz/$commit" \
        -o "$tmp/$repo.tar.gz"
    if [[ $(checksum <"$tmp/$repo.tar.gz") != "$sha256" ]]; then
        echo "vendor-projectm: checksum mismatch for $repo at $commit" >&2
        exit 1
    fi
    mkdir -p "$dest"
    # GitHub wraps the tree in a <repo>-<commit> directory; strip it.
    tar -xzf "$tmp/$repo.tar.gz" -C "$dest" --strip-components=1
}

rm -rf "$out"
mkdir -p vendor
fetch projectm "$projectm_commit" "$projectm_sha256" "$out"
# The submodule path from projectM's .gitmodules. The build falls back to
# these sources when ENABLE_SYSTEM_PROJECTM_EVAL is off, so an empty
# directory here is a link error a long way downstream.
fetch projectm-eval "$eval_commit" "$eval_sha256" "$out/vendor/projectm-eval"

echo "$want" >"$stamp"
echo "vendor-projectm: projectM $projectm_commit vendored into $out"
