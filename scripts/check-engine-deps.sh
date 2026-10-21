#!/usr/bin/env bash
# Keep bundled SQLite out of TinyMemory's default and full product graphs.
set -euo pipefail

readonly repo_root="$(cd -- "$(dirname -- "$0")/.." && pwd)"
cd -- "$repo_root"

has_forbidden_engine() {
  awk '
    {
      name = $1
      sub(/\r$/, "", name)
      if (name == "rusqlite" || name == "libsqlite3-sys") found = 1
    }
    END { exit found ? 0 : 1 }
  ' <<< "$1"
}

metadata_library_packages() {
  jq -r '
    . as $metadata
    | [.packages[]
       | select(.id as $id | $metadata.workspace_members | index($id))
       | select(any(.targets[];
           any(.kind[]; . == "lib" or . == "rlib" or . == "dylib" or
               . == "cdylib" or . == "staticlib" or . == "proc-macro")))
       | .name]
    | unique
    | .[]
  '
}

fail() {
  echo "engine dependency check: $*" >&2
  exit 1
}

self_test() {
  local synthetic_metadata synthetic_libraries expected_libraries
  synthetic_metadata='{"workspace_members":["member-lib","member-rlib","member-dylib","member-cdylib","member-staticlib","member-proc"],"packages":[{"id":"member-lib","name":"lib-kind-lib","targets":[{"kind":["lib"]}]},{"id":"member-rlib","name":"lib-kind-rlib","targets":[{"kind":["rlib"]}]},{"id":"member-dylib","name":"lib-kind-dylib","targets":[{"kind":["dylib"]}]},{"id":"member-cdylib","name":"lib-kind-cdylib","targets":[{"kind":["cdylib"]}]},{"id":"member-staticlib","name":"lib-kind-staticlib","targets":[{"kind":["staticlib"]}]},{"id":"member-proc","name":"lib-kind-proc-macro","targets":[{"kind":["proc-macro"]}]},{"id":"external-lib","name":"external-library","targets":[{"kind":["lib"]}]},{"id":"non-member","name":"non-member-library","targets":[{"kind":["lib"]}]}]}'
  synthetic_libraries="$(printf '%s\r\n' "$synthetic_metadata" | metadata_library_packages | tr -d '\r' | sort)"
  expected_libraries="$(printf '%s\n' \
    lib-kind-lib lib-kind-rlib lib-kind-dylib lib-kind-cdylib \
    lib-kind-staticlib lib-kind-proc-macro | sort)"
  [[ "$synthetic_libraries" == "$expected_libraries" ]] || \
    fail "metadata self-test did not enumerate workspace libraries across kinds/CRLF"
  if [[ -n "$(printf '%s\r\n' '{"workspace_members":[],"packages":[]}' | metadata_library_packages)" ]]; then
    fail "empty metadata self-test produced a library package"
  fi
  if has_forbidden_engine $'rusqlite\r v0.40.0'; then
    :
  else
    fail "CRLF package-name self-test did not detect rusqlite"
  fi
  if has_forbidden_engine $'libsqlite3-sys\r v0.30.1'; then
    :
  else
    fail "CRLF package-name self-test did not detect libsqlite3-sys"
  fi
  if has_forbidden_engine $'tinyredact\r v2.0.0\nmy-rusqlite v1.0.0'; then
    fail "anchored package-name self-test matched a benign package"
  fi
  echo "engine dependency check self-test: ok"
}

if [[ "${1:-}" == "--self-test" ]]; then
  self_test
  exit 0
fi
if [[ "$#" -ne 0 ]]; then
  fail "usage: $0 [--self-test]"
fi

metadata="$(cargo metadata --locked --format-version 1 --no-deps)"
library_packages="$(metadata_library_packages <<< "$metadata" | tr -d '\r')"
[[ -n "$library_packages" ]] || fail "workspace metadata contained no library targets"
grep -Fxq tinymemory-integrations <<< "$library_packages" || \
  fail "workspace metadata omitted tinymemory-integrations"

check_tree() {
  local package="$1"
  local label="$2"
  shift
  shift
  local tree
  tree="$(cargo tree --locked -p "$package" --edges normal,build --prefix none "$@")"
  [[ -n "$tree" ]] || fail "$package $label tree was empty"
  if has_forbidden_engine "$tree"; then
    printf '%s\n' "$tree" | awk '
      { name = $1; sub(/\r$/, "", name)
        if (name == "rusqlite" || name == "libsqlite3-sys") print }
    ' >&2
    fail "$package $label graph contains bundled SQLite"
  fi
  echo "engine dependency check: $package $label graph is SQLite-free"
}

self_test
for package in $library_packages; do
  check_tree "$package" "default"
  if [[ "$package" == "tinymemory-integrations" ]]; then
    check_tree "$package" "full product" --no-default-features --features full
  else
    check_tree "$package" "all features" --all-features
  fi
done

# The product tree above deliberately excludes the legacy importer, while this
# positive control ensures the explicitly selected migration feature still
# brings its optional SQLite reader with it.
legacy_tree="$(cargo tree --locked -p tinymemory-integrations --no-default-features \
  --features legacy-import --edges normal,build --prefix none)"
[[ -n "$legacy_tree" ]] || fail "explicit legacy-import tree was empty"
has_forbidden_engine "$legacy_tree" || \
  fail "explicit legacy-import graph no longer contains its SQLite reader"
echo "engine dependency check: explicit legacy-import retains SQLite"
