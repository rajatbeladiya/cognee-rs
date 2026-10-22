#!/usr/bin/env bash
# Fail if an ELF binary / shared object needs a newer glibc or libstdc++ than
# the floor we promise for its target.
#
#   ci/assert-glibc-floor.sh --target <rust-target> <elf>...
#       floors from ci/linux-glibc-floor.txt (the shipped-artifact promise)
#   ci/assert-glibc-floor.sh --glibc X.YY [--glibcxx X.Y.ZZ] <elf>...
#       explicit floors
#
# Reads the versioned *undefined* dynamic symbols (`objdump -T`, `*UND*` rows)
# and takes the highest GLIBC_x.y and GLIBCXX_x.y.z any of them references.
# That is the oldest glibc / libstdc++ the file will load against: the dynamic
# loader refuses a file that references a symbol version the system library
# does not define ("version `GLIBC_2.43' not found"). Defined symbols are
# skipped on purpose — a file that bundles libstdc++ statically *exports*
# GLIBCXX_* versions, which say nothing about the system it needs.
#
# Why this exists: a native Linux build takes its glibc floor from whatever
# image it happens to run on. On 2026-10-22 ubuntu-latest moved to 26.04
# (glibc 2.43) and binaries built there stopped loading on older systems,
# with nothing in the build to say so. This makes the floor a failing
# assertion on the artifact itself.
#
# objdump (binutils) reads foreign-arch ELF — aarch64 files on an x86_64
# runner — without a cross toolchain.
set -euo pipefail

usage() {
    echo "usage: $0 --target <rust-target> <elf>..." >&2
    echo "       $0 --glibc X.YY [--glibcxx X.Y.ZZ] <elf>..." >&2
    exit 2
}

FLOOR_FILE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/linux-glibc-floor.txt"

max_glibc=""
max_glibcxx=""
while [ $# -gt 0 ]; do
    case "$1" in
        --target)
            [ $# -ge 2 ] || usage
            row=$(awk -v t="$2" '$0 !~ /^#/ && $1 == t { print $2, $3 }' "$FLOOR_FILE")
            if [ -z "$row" ]; then
                echo "::error::no floor for target '$2' in $FLOOR_FILE" >&2
                exit 2
            fi
            read -r max_glibc max_glibcxx <<<"$row"
            shift 2
            ;;
        --glibc)   [ $# -ge 2 ] || usage; max_glibc="$2"; shift 2 ;;
        --glibcxx) [ $# -ge 2 ] || usage; max_glibcxx="$2"; shift 2 ;;
        --) shift; break ;;
        -*) usage ;;
        *) break ;;
    esac
done
[ -n "$max_glibc" ] && [ $# -ge 1 ] || usage

# Highest version on stdin (sort -V is version-aware: 2.9 < 2.17 < 2.38).
highest() { sort -V | tail -n1; }

# version_gt A B: true when A is strictly newer than B.
version_gt() { [ "$1" != "$2" ] && [ "$(printf '%s\n%s\n' "$1" "$2" | highest)" = "$1" ]; }

# list_above PREFIX FLOOR: the imported PREFIX_<ver> symbols newer than FLOOR.
# Reads the *UND* rows on stdin; their last two columns are e.g.
# `(GLIBC_2.38) __isoc23_strtol` (parenthesised when the version is hidden).
list_above() {
    awk '{ print $(NF-1), $NF }' | tr -d '()' |
        { grep -E "^$1_[0-9]" || true; } | sed "s/^$1_//" | sort -uV |
        while read -r v sym; do
            if version_gt "$v" "$2"; then echo "    $1_$v $sym" >&2; fi
        done
}

fail=0
for f in "$@"; do
    if [ ! -f "$f" ]; then
        echo "::error::$f: no such file" >&2
        fail=1
        continue
    fi
    if ! dyn=$(objdump -T "$f" 2>&1); then
        echo "::error::$f: objdump cannot read its dynamic symbols — not an ELF shared object/executable? ($dyn)" >&2
        fail=1
        continue
    fi
    und=$(printf '%s\n' "$dyn" | awk '/\*UND\*/')
    need_glibc=$(printf '%s\n' "$und" | { grep -oE 'GLIBC_[0-9]+(\.[0-9]+)+' || true; } |
        sed 's/^GLIBC_//' | highest)
    need_glibcxx=$(printf '%s\n' "$und" | { grep -oE 'GLIBCXX_[0-9]+(\.[0-9]+)+' || true; } |
        sed 's/^GLIBCXX_//' | highest)

    if [ -z "$need_glibc" ]; then
        # Every dynamically-linked glibc object imports at least one versioned
        # glibc symbol; none means objdump read nothing useful (a static
        # binary, the wrong file, or an objdump that cannot parse it).
        echo "::error::$f: no GLIBC_* imports found — is this a dynamically-linked glibc ELF?" >&2
        fail=1
        continue
    fi

    if version_gt "$need_glibc" "$max_glibc"; then
        echo "::error::$f needs GLIBC_$need_glibc, newer than the promised floor GLIBC_$max_glibc. Imports above the floor:" >&2
        printf '%s\n' "$und" | list_above GLIBC "$max_glibc"
        fail=1
    else
        echo "ok: $f needs GLIBC_$need_glibc (floor $max_glibc)"
    fi

    [ -n "$max_glibcxx" ] || continue
    if [ -z "$need_glibcxx" ]; then
        echo "ok: $f imports no GLIBCXX_* symbols (libstdc++ static or unused)"
    elif version_gt "$need_glibcxx" "$max_glibcxx"; then
        echo "::error::$f needs GLIBCXX_$need_glibcxx, newer than the promised floor GLIBCXX_$max_glibcxx. Imports above the floor:" >&2
        printf '%s\n' "$und" | list_above GLIBCXX "$max_glibcxx"
        fail=1
    else
        echo "ok: $f needs GLIBCXX_$need_glibcxx (floor $max_glibcxx)"
    fi
done
exit "$fail"
