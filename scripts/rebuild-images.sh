#!/usr/bin/env bash
# Rebuild the boot images in boot/ (plan/09): core.scm and the compiler,
# compiled by the compiler in Scheme. Each round compiles both with the
# images the last round made, until a round changes nothing: a fixed
# point. Then s3 is built again with them embedded.
#
#   scripts/rebuild-images.sh
#
# The current images must still load: a change to the instruction set or
# the compiled-file format needs a binary that reads the old images and
# writes the new (or, as a last resort, tag s2-final's Rust compiler).

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

cargo build --release
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cp boot/s3-core.s3o boot/s3-compiler.s3o "$work"

for round in 1 2 3 4 5; do
    export S3_CORE_IMAGE="$work/s3-core.s3o" S3_COMPILER_IMAGE="$work/s3-compiler.s3o"
    target/release/s3 --core-image "$work/core.new"
    target/release/s3 --compiler-image "$work/compiler.new"
    if cmp -s "$work/core.new" "$S3_CORE_IMAGE" && cmp -s "$work/compiler.new" "$S3_COMPILER_IMAGE"; then
        cp "$work/s3-core.s3o" "$work/s3-compiler.s3o" boot/
        unset S3_CORE_IMAGE S3_COMPILER_IMAGE
        cargo build --release
        echo "boot images at a fixed point after $round round(s)"
        exit 0
    fi
    mv "$work/core.new" "$S3_CORE_IMAGE"
    mv "$work/compiler.new" "$S3_COMPILER_IMAGE"
done
echo "no fixed point after 5 rounds; boot/ unchanged" >&2
exit 1
