#!/usr/bin/env bash
set -euo pipefail
cd /workspace

# Lockfiles + Node version invalidate dependencies, including reused volumes.
install_deps() {
  local folder="$1" fingerprint
  fingerprint=$(cd "$folder" && { cat package.json package-lock.json; node --version; } | sha256sum | cut -d ' ' -f 1)
  if [[ ! -f "$folder/node_modules/.deeptutor-lock" ]] || \
     [[ "$(cat "$folder/node_modules/.deeptutor-lock")" != "$fingerprint" ]]; then
    (cd "$folder" && npm ci --legacy-peer-deps --no-audit --no-fund)
    printf '%s\n' "$fingerprint" > "$folder/node_modules/.deeptutor-lock"
  fi
}
install_deps web
install_deps packages/browser-testing
install_deps packages/video-lesson

# Compile before starting Next so their startup memory peaks do not overlap.
cargo build --locked -p deeptutor-server --bin deeptutor-server

backend_pid=""
web_pid=""
cleanup() {
  trap - EXIT INT TERM
  [[ -z "$web_pid" ]] || kill -TERM "$web_pid" 2>/dev/null || true
  [[ -z "$backend_pid" ]] || kill -TERM "$backend_pid" 2>/dev/null || true
  wait || true
}
trap cleanup EXIT
trap 'exit 0' INT TERM

target/debug/deeptutor-server --home "$DEEPTUTOR_HOME" serve --port 8002 &
backend_pid=$!
node --input-type=module - <<'JS'
for (let attempt = 0; attempt < 60; attempt++) {
  try {
    const response = await fetch('http://127.0.0.1:8002/health/ready');
    if (response.ok) process.exit(0);
  } catch {}
  await new Promise(resolve => setTimeout(resolve, 500));
}
throw new Error('Native backend did not become ready within 30 seconds');
JS

cd web
node scripts/copy-pdfjs-assets.mjs
node scripts/copy-voice-assets.mjs
case "${DEEPTUTOR_DEV_BUNDLER:-webpack}" in
  webpack) args=(--low-memory) ;;
  turbopack) args=(--turbopack) ;;
  *) echo "DEEPTUTOR_DEV_BUNDLER must be webpack or turbopack" >&2; exit 1 ;;
esac
node scripts/dev.mjs "${args[@]}" --hostname 0.0.0.0 --port 3782 &
web_pid=$!
# Either process exiting stops its sibling instead of leaving a half-live service.
set +e
wait -n "$backend_pid" "$web_pid"
status=$?
set -e
exit "$status"
