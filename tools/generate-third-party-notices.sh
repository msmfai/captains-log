#!/usr/bin/env bash
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
output="${1:-$repo/THIRD_PARTY_NOTICES.txt}"
temporary="$(mktemp)"
trap 'rm -f "$temporary"' EXIT

metadata="$(
  cargo metadata \
    --format-version 1 \
    --filter-platform aarch64-apple-darwin \
    --offline
)"

{
  echo "Captain's Log — Third-Party Notices"
  echo
  echo "This file is generated from the exact Cargo dependency graph for the"
  echo "aarch64-apple-darwin build and other third-party artifacts distributed"
  echo "in this repository. Each section records the package's declared licence"
  echo "expression and reproduces its packaged licence or notice files."
  echo
} > "$temporary"

while IFS=$'\t' read -r name version manifest license source; do
  [ "$source" != "workspace" ] || continue
  directory="${manifest%/Cargo.toml}"
  mapfile=()
  while IFS= read -r file; do
    mapfile+=("$file")
  done < <(
    find "$directory" -maxdepth 2 -type f \
      \( -iname 'LICENSE*' -o -iname 'COPYING*' -o -iname 'NOTICE*' -o -iname 'COPYRIGHT*' \) \
      | LC_ALL=C sort
  )

  {
    echo "================================================================================"
    printf '%s %s\n' "$name" "$version"
    printf 'Declared licence: %s\n' "$license"
    printf 'Source: %s\n\n' "$source"
  } >> "$temporary"

  if [ "${#mapfile[@]}" -eq 0 ]; then
    case "$name" in
      block2|dispatch2|objc-sys|objc2|objc2-*)
        cat "$repo/third-party/objc2-LICENSE-MIT.txt" >> "$temporary"
        ;;
      *)
        echo "No licence or notice file was packaged with $name $version." >&2
        exit 1
        ;;
    esac
  else
    for file in "${mapfile[@]}"; do
      printf '%s\n\n' "--- ${file##*/} ---" >> "$temporary"
      cat "$file" >> "$temporary"
      printf '\n\n' >> "$temporary"
    done
  fi
done < <(
  jq -r '
    .packages
    | sort_by(.name, .version)
    | .[]
    | [
        .name,
        .version,
        .manifest_path,
        (.license // "NO-LICENCE-FIELD"),
        (.source // "workspace")
      ]
    | @tsv
  ' <<< "$metadata"
)

gradle_wrapper="$repo/android/gradle/wrapper/gradle-wrapper.jar"
gradle_version="$(sed -n 's#.*gradle-\([0-9][0-9.]*\)-bin\.zip#\1#p' \
  "$repo/android/gradle/wrapper/gradle-wrapper.properties")"
if [ ! -s "$gradle_wrapper" ] || ! unzip -p "$gradle_wrapper" META-INF/LICENSE >/dev/null; then
  echo "Gradle wrapper or its packaged licence is missing" >&2
  exit 1
fi
{
  echo "================================================================================"
  printf 'Gradle Wrapper %s\n' "$gradle_version"
  echo "Declared licence: Apache-2.0"
  echo "Source: android/gradle/wrapper/gradle-wrapper.jar"
  echo
  echo "--- META-INF/LICENSE ---"
  echo
  unzip -p "$gradle_wrapper" META-INF/LICENSE
  echo
} >> "$temporary"

{
  echo "================================================================================"
  echo "Harmony Contributor Agreement Template 1.0"
  echo "Declared licence: CC-BY-3.0"
  echo "Source: https://www.harmonyagreements.org/agreements"
  echo
  echo "The Individual and Entity Contributor License Agreements are adapted from the"
  echo "Harmony templates by selecting the Contributor License Agreement, outbound"
  echo "Option Five, and filling the project, legal entity, submission, non-owner and"
  echo "governing-law fields."
  echo
  echo "Licence: https://creativecommons.org/licenses/by/3.0/"
  echo
} >> "$temporary"

mv "$temporary" "$output"
trap - EXIT
