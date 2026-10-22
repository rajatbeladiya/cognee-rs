#!/usr/bin/env bash
# Self-test for ci/assert-glibc-floor.sh, run in ci.yml / community.yml lint.
#
# The script's real callers are the Linux legs of ts-prebuild, java-prebuild
# and capi-release, which only run on tags and on PRs touching their build
# environment. This exercises it against the runner's own /bin/ls on every
# PR, using floors chosen so the result does not depend on the image:
# every glibc binary needs more than 2.2 and less than 99.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
assert="$here/assert-glibc-floor.sh"
bin=/bin/ls

expect_fail() {
    if "$@" >/dev/null 2>&1; then
        echo "FAIL: expected failure: $*" >&2
        exit 1
    fi
    echo "ok (failed as expected): $*"
}

# Passes under a floor above anything real.
bash "$assert" --glibc 99.0 "$bin"
# Fails under a floor below anything real.
expect_fail bash "$assert" --glibc 2.2 "$bin"
# Every target in the floor file resolves (an explicit --glibc after
# --target overrides the file's value, keeping this image-independent).
while read -r target _; do
    bash "$assert" --target "$target" --glibc 99.0 "$bin" >/dev/null
    echo "ok: floor file has a row for $target"
done < <(awk '$0 !~ /^#/ && NF { print $1 }' "$here/linux-glibc-floor.txt")
# An unknown target, a missing file and a non-ELF file are errors, not passes.
expect_fail bash "$assert" --target no-such-target "$bin"
expect_fail bash "$assert" --glibc 99.0 /nonexistent
expect_fail bash "$assert" --glibc 99.0 "$here/linux-glibc-floor.txt"
echo "assert-glibc-floor.sh self-test passed"
