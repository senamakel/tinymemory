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
      if (name == "rusqlite" || name == "libsqlite3-sys" ||
          name == "mongodb" || name == "bson" || name == "aws-sdk-s3" ||
          name == "lapin" || name == "sqlx" || name == "redb" ||
          name == "sled" || name == "rocksdb" ||
          (name ~ /^tinystoragedrivers(-|$)/ &&
           name != "tinystoragedrivers-api" && name != "tinystoragedrivers-types" &&
           name != "tinystoragedrivers-bus" && name != "tinystoragedrivers-busproxy")) found = 1
    }
    END { exit found ? 0 : 1 }
  ' <<< "$1"
}

has_sqlite_reader() {
  awk '
    { name = $1; sub(/\r$/, "", name)
      if (name == "rusqlite" || name == "libsqlite3-sys") found = 1 }
    END { exit found ? 0 : 1 }
  ' <<< "$1"
}

tree_has_root() {
  local expected="$1"
  awk -v expected="$expected" '
    { name = $1; sub(/\r$/, "", name); if (name == expected) found = 1 }
    END { exit found ? 0 : 1 }
  ' <<< "$2"
}

run_cargo_with() {
  local runner="$1"
  shift
  local output
  if ! output="$("$runner" "$@")"; then
    echo "cargo command failed: $runner $*" >&2
    return 1
  fi
  printf '%s\n' "$output"
}

run_cargo() { run_cargo_with cargo "$@"; }

metadata_library_packages() {
  jq -r '
    . as $metadata
    | if (($metadata | type) != "object") or (($metadata.packages | type) != "array") or
         (($metadata.workspace_members | type) != "array") or
         (($metadata.workspace_members | length) == 0)
      then error("missing workspace metadata") else . end
    | if any(.workspace_members[]; . as $id |
         ([$metadata.packages[] | select(.id == $id)] | length) != 1)
      then error("workspace member ID missing or duplicated in package metadata") else . end
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
  if metadata_library_packages <<< '{"workspace_members":[],"packages":[]}' >/dev/null 2>&1; then
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
  has_sqlite_reader $'rusqlite\r v0.40.0' || \
    fail "SQLite positive-control self-test did not detect rusqlite"
  has_sqlite_reader $'libsqlite3-sys\r v0.30.1' || \
    fail "SQLite positive-control self-test did not detect libsqlite3-sys"
  if has_sqlite_reader $'mongodb v3.0.0\nnot-rusqlite v1.0.0'; then
    fail "SQLite positive-control self-test matched a different package"
  fi
  if has_forbidden_engine $'tinyredact\r v2.0.0\nmy-rusqlite v1.0.0'; then
    fail "anchored package-name self-test matched a benign package"
  fi
  for engine in mongodb bson aws-sdk-s3 lapin sqlx redb sled rocksdb \
      tinystoragedrivers-document tinystoragedrivers-sqlite; do
    has_forbidden_engine "$engine v1.0.0" || \
      fail "positive-control self-test did not detect $engine"
  done
  if has_forbidden_engine $'tinystoragedrivers-api\r v2.0.2\ntinystoragedrivers-types v2.0.2\ntinystoragedrivers-bus v2.0.2\ntinystoragedrivers-busproxy v2.0.2'; then
    fail "allowed TSD contract packages were rejected"
  fi
  if metadata_library_packages <<< '{"packages":[],"workspace_members":["missing"]}' >/dev/null 2>&1; then
    fail "metadata self-test accepted a missing workspace member ID"
  fi
  if metadata_library_packages <<< 'not json' >/dev/null 2>&1; then
    fail "metadata self-test accepted malformed JSON"
  fi
  if tree_has_root tinymemory-integrations $'other-package\r v1.0.0'; then
    fail "tree self-test accepted output without the requested root"
  fi
  if run_cargo_with false metadata >/dev/null 2>&1; then
    fail "cargo-command self-test accepted a failed command"
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

metadata="$(run_cargo metadata --locked --format-version 1 --no-deps)" || \
  fail "cargo metadata failed"
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
  tree="$(run_cargo tree --locked -p "$package" --edges normal,build --prefix none "$@")" || \
    fail "$package $label cargo tree failed"
  [[ -n "$tree" ]] || fail "$package $label tree was empty"
  tree_has_root "$package" "$tree" || fail "$package $label tree omitted its root package"
  if has_forbidden_engine "$tree"; then
    printf '%s\n' "$tree" | awk '
      { name = $1; sub(/\r$/, "", name)
        if (name == "rusqlite" || name == "libsqlite3-sys" ||
            name == "mongodb" || name == "bson" || name == "aws-sdk-s3" ||
            name == "lapin" || name == "sqlx" || name == "redb" ||
            name == "sled" || name == "rocksdb" ||
            (name ~ /^tinystoragedrivers(-|$)/ &&
             name != "tinystoragedrivers-api" && name != "tinystoragedrivers-types" &&
             name != "tinystoragedrivers-bus" && name != "tinystoragedrivers-busproxy")) print }
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
tree_has_root tinymemory-integrations "$legacy_tree" || \
  fail "explicit legacy-import tree omitted its root package"
has_sqlite_reader "$legacy_tree" || \
  fail "explicit legacy-import graph no longer contains its SQLite reader"
echo "engine dependency check: explicit legacy-import retains SQLite"
