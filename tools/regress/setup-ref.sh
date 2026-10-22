#!/bin/sh
# Extract a reference revision of kurbo-se for the regression suite.
#
#   tools/regress/setup-ref.sh <git-rev>
#
# The revision's tree goes to target/regress-ref/ with its package renamed to
# kurbo-se-ref, so it links into one binary next to the working tree.
set -eu
rev=${1:?usage: setup-ref.sh <git-rev>}
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
dest="$repo/target/regress-ref"
hash=$(git -C "$repo" rev-parse --verify "$rev^{commit}")
rm -rf "$dest"
mkdir -p "$dest"
git -C "$repo" archive "$hash" | tar -x -C "$dest"
sed 's/^name = "kurbo-se"$/name = "kurbo-se-ref"/' "$dest/Cargo.toml" > "$dest/Cargo.toml.tmp"
mv "$dest/Cargo.toml.tmp" "$dest/Cargo.toml"
grep -q '^name = "kurbo-se-ref"$' "$dest/Cargo.toml" || {
    echo "setup-ref.sh: could not rename the package in $dest/Cargo.toml" >&2
    exit 1
}
echo "$hash" > "$dest/REGRESS_REF"
echo "reference $hash extracted to $dest"
