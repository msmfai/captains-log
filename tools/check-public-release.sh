#!/usr/bin/env bash
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo"

if [ "$#" -ne 0 ]; then
  echo "usage: $0" >&2
  exit 2
fi

if [ -e LICENSE-DECISION.md ]; then
  echo "obsolete license-decision blocker remains after license selection" >&2
  exit 1
fi
if [ ! -f LICENSE ]; then
  echo "release blocked: LICENSE is missing" >&2
  exit 1
fi
expected_agpl_sha256="0d96a4ff68ad6d4b6f1f30f713b18d5184912ba8dd389f86aa7710db079abcb0"
if [ "$(shasum -a 256 LICENSE | awk '{print $1}')" != "$expected_agpl_sha256" ]; then
  echo "LICENSE must be the unmodified official AGPL-3.0 text" >&2
  exit 1
fi

candidate_files="$({
  git ls-files --cached --others --exclude-standard
} | while IFS= read -r path; do
  [ -f "$path" ] && printf '%s\n' "$path"
done)"

if rg -n '\.(aac|flac|m4a|mp3|ogg|opus|wav|sqlite3?|db|parquet|lance|ndjson|jsonl|apk|aab|dmg|ipa|pkg|jks|keystore|p12|mobileprovision)$' \
    <<<"$candidate_files"; then
  echo "candidate contains private data or a generated package" >&2
  exit 1
fi

surface=(Cargo.toml README.md CHANGELOG.md CODE_OF_CONDUCT.md CONTRIBUTING.md SECURITY.md \
  LICENSING.md CLA architecture.json config.example.toml .github crates docs packaging tools android)
if rg -n 'PKT01_[A-Z]+_[0-9A-Fa-f]{8}' "${surface[@]}" --glob '!android/**/build/**'; then
  echo "candidate contains a recorder identifier rather than a synthetic fixture" >&2
  exit 1
fi
if rg -n '/Users/[^/ ]+|AKIA[0-9A-Z]{16}|gh[pousr]_[A-Za-z0-9]{30,}|glpat-[A-Za-z0-9_-]{20,}|xox[baprs]-[A-Za-z0-9-]{20,}|-----BEGIN (RSA |EC |OPENSSH )?PRIVATE KEY-----|Bearer [A-Za-z0-9._~-]{20,}|https?://[^/[:space:]]+:[^/@[:space:]]+@' \
  "${surface[@]}" --glob '!android/**/build/**' --glob '!tools/check-public-release.sh'; then
  echo "personal path or likely credential found" >&2
  exit 1
fi

if rg -n -i 'private GitLab|configured origin|release/public|worktree convention|agentic control-plane|reverse.engineering narrative|takedown surface' \
    README.md CHANGELOG.md CODE_OF_CONDUCT.md CONTRIBUTING.md SECURITY.md docs .github; then
  echo "private development or internal release narrative found" >&2
  exit 1
fi

core=(
  crates/personal-audio-lake-contracts
  crates/personal-audio-lake-storage
  crates/personal-audio-lake-transcription
  crates/personal-audio-lake-workflows
  crates/personal-audio-lake-services
  crates/personal-audio-lake-app/src/api.rs
  crates/personal-audio-lake-app/src/read_model.rs
  crates/personal-audio-lake-app/src/view_model.rs
  crates/personal-audio-lake-app/src/app_controller.rs
)
if rg -n 'Pocket|pocket|GATT|Bluetooth|service_uuid|control_uuid|binding_key' "${core[@]}"; then
  echo "source-specific protocol leaked into an inward-facing crate" >&2
  exit 1
fi

if rg -n 'DebugMatrix|debug_matrix|APP&D&|packet capture|firmware inspection|app inspection' \
    crates android/app/src docs README.md; then
  echo "forbidden diagnostic surface found" >&2
  exit 1
fi

if rg -n '001120a[0-3]-2233-4455-6677-889912345678|APP&[A-Z]|MCU&[A-Z]' \
    README.md docs crates android/app/src \
    --glob '!crates/personal-audio-lake-pocket/**' --glob '!android/app/src/**/Pocket*'; then
  echo "raw compatibility protocol details escaped their leaf implementation" >&2
  exit 1
fi

if ! rg -q '^license = "AGPL-3\.0-only"$' Cargo.toml; then
  echo "workspace license metadata must be AGPL-3.0-only" >&2
  exit 1
fi
public_repository="https://github.com/msmfai/captains-log"
if ! rg -F -q "repository = \"$public_repository\"" Cargo.toml ||
   ! rg -F -q "homepage = \"$public_repository\"" Cargo.toml ||
   ! rg -F -q "]($public_repository)" README.md; then
  echo "canonical public source URL is missing from package metadata or README" >&2
  exit 1
fi
package_count="$(rg --files crates -g Cargo.toml | wc -l | tr -d '[:space:]')"
blocked_package_count="$(rg -l '^publish = false$' crates/*/Cargo.toml | wc -l | tr -d '[:space:]')"
if [ "$package_count" != "$blocked_package_count" ]; then
  echo "every workspace package must block accidental crates.io publication" >&2
  exit 1
fi
licensed_package_count="$(rg -l '^license\.workspace = true$' crates/*/Cargo.toml | wc -l | tr -d '[:space:]')"
if [ "$package_count" != "$licensed_package_count" ]; then
  echo "every workspace package must inherit AGPL-3.0-only metadata" >&2
  exit 1
fi
repository_package_count="$(rg -l '^repository\.workspace = true$' crates/*/Cargo.toml | wc -l | tr -d '[:space:]')"
homepage_package_count="$(rg -l '^homepage\.workspace = true$' crates/*/Cargo.toml | wc -l | tr -d '[:space:]')"
if [ "$package_count" != "$repository_package_count" ] ||
   [ "$package_count" != "$homepage_package_count" ]; then
  echo "every workspace package must inherit the canonical public source URL" >&2
  exit 1
fi
for agreement in CLA/individual.md CLA/entity.md; do
  if ! rg -F -q 'Solomonoff Consultancy Ltd' "$agreement" ||
     ! rg -F -q 'company number 16208793' "$agreement" ||
     ! rg -F -q 'Harmony Option Five' "$agreement" ||
     ! rg -F -q 'laws of England and Wales' "$agreement"; then
    echo "Harmony Option Five CLA identity or governing law is incomplete: $agreement" >&2
    exit 1
  fi
done
if ! rg -F -q 'separately negotiate a commercial licence' LICENSING.md; then
  echo "commercial licensing path is missing" >&2
  exit 1
fi

if ! rg -F -q \
    'independently developed and is not made, sponsored,' README.md ||
   ! rg -F -q \
    'responsible for giving any required notice' docs/privacy.md; then
  echo "non-affiliation or recording-consent notice is missing" >&2
  exit 1
fi

if ! rg -F -q '# Captain’s Log' README.md ||
   ! rg -F -q '<key>CFBundleDisplayName</key><string>Captain’s Log</string>' packaging/Info.plist ||
   ! rg -F -q '<string name="app_name">Captain’s Log</string>' \
      android/app/src/main/res/values/strings.xml; then
  echo "Captain’s Log public branding is incomplete" >&2
  exit 1
fi

if [ ! -s THIRD_PARTY_NOTICES.txt ]; then
  echo "third-party notices are missing" >&2
  exit 1
fi

if ! rg -q 'tools/verify-reproducible-artifacts\.sh' tools/verify-all.sh ||
   ! rg -q ':app:testReleaseUnitTest :app:assembleRelease :app:lintRelease' tools/verify-all.sh; then
  echo "complete verification must include reproducible artifacts and the Android release variant" >&2
  exit 1
fi
if rg -n '\bgradle --no-daemon' tools; then
  echo "Android verification must use the repository's pinned Gradle wrapper" >&2
  exit 1
fi
if ! rg -q '^distributionSha256Sum=f1771298a70f6db5a29daf62378c4e18a17fc33c9ba6b14362e0cdf40610380d$' \
    android/gradle/wrapper/gradle-wrapper.properties ||
   [ "$(shasum -a 256 android/gradle/wrapper/gradle-wrapper.jar | awk '{print $1}')" != \
     "7d3a4ac4de1c32b59bc6a4eb8ecb8e612ccd0cf1ae1e99f66902da64df296172" ]; then
  echo "Gradle distribution or wrapper JAR integrity pin is missing or incorrect" >&2
  exit 1
fi
if ! rg -q 'CAPTAINS_LOG_CLICKHOUSE_VERSION:-26\.6\.2\.63' tools/install-runtime.sh ||
   ! rg -q 'selected_clickhouse.*clickhouse_version' tools/install-runtime.sh; then
  echo "runtime setup must pin and verify the tested ClickHouse server version" >&2
  exit 1
fi
if ! rg -q '\./tools/verify-all\.sh' .github/workflows/verify.yml; then
  echo "GitHub verification must call the same complete local release gate" >&2
  exit 1
fi
for required in \
  LICENSE LICENSING.md CLA/individual.md CLA/entity.md \
  CHANGELOG.md CODE_OF_CONDUCT.md CONTRIBUTING.md SECURITY.md \
  .github/PULL_REQUEST_TEMPLATE.md \
  .github/ISSUE_TEMPLATE/bug_report.yml \
  .github/ISSUE_TEMPLATE/feature_request.yml \
  .github/ISSUE_TEMPLATE/config.yml \
  .github/dependabot.yml \
  docs/releasing.md docs/release-notes-0.1.0.md docs/troubleshooting.md \
  docs/release-checklist.md docs/verification-report.md \
  tools/verify-public-history.sh; do
  if [ ! -s "$required" ]; then
    echo "required public repository file is missing or empty: $required" >&2
    exit 1
  fi
done
if ! rg -q 'persist-credentials: false' .github/workflows/verify.yml ||
   ! rg -q 'permissions:' .github/workflows/verify.yml ||
   ! rg -q 'contents: read' .github/workflows/verify.yml; then
  echo "GitHub Actions must retain least-privilege checkout settings" >&2
  exit 1
fi

while IFS= read -r document; do
  directory="$(dirname "$document")"
  while IFS= read -r link; do
    case "$link" in
      http:*|https:*|mailto:*|'#'*) continue ;;
    esac
    path="${link%%#*}"
    path="${path%%\?*}"
    path="${path#<}"
    path="${path%>}"
    [ -z "$path" ] && continue
    if [ ! -e "$repo/$directory/$path" ]; then
      echo "broken repository link in $document: $link" >&2
      exit 1
    fi
  done < <(rg -o --no-filename '\]\([^)]+' "$document" | sed 's/^](//' || true)
done < <(printf '%s\n' "$candidate_files" | rg '\.md$')

git diff --check
echo "public-release surface: privacy, coupling, notices, links, and metadata verified"
