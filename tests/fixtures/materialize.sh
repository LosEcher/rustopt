#!/bin/sh
# Copy a fixture package to a temporary directory, give it a real Cargo.toml, and
# print the directory. Fixture manifests are stored as `manifest.toml` because
# cargo excludes any subdirectory containing a Cargo.toml from the package.
#
#   dir=$(bash tests/fixtures/materialize.sh tiny)
#   cargo run -- plan --manifest "$dir"
set -eu

name="${1:?usage: materialize.sh <fixture-name> [dest-dir]}"
here=$(cd "$(dirname "$0")" && pwd)
src="$here/$name"
[ -d "$src" ] || { echo "no such fixture: $name" >&2; exit 2; }

dest="${2:-$(mktemp -d "${TMPDIR:-/tmp}/rustopt-fixture-$name.XXXXXX")}"
mkdir -p "$dest"
cp -R "$src/." "$dest/"
mv "$dest/manifest.toml" "$dest/Cargo.toml"
rm -f "$dest/Cargo.lock"
echo "$dest"
