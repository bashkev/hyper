#!/bin/sh
# Builds target/Hyper.app: the release binary with its icon. `--open` also launches it.
# `--dev` builds target/Hyper Dev.app instead, a separate copy that can be
# restarted without touching the terminals in your everyday Hyper.
# `--reload` has a running copy take the new build over in place, keeping its terminals
# and the programs in them, or opens one if none is running.
set -eu
cd "$(dirname "$0")/.."

# HYPER_BUNDLE_ID, or `git config hyper.bundleId` for one checkout, gives your own build its
# own identifier; --dev appends `.dev` to it.
bundle_id=${HYPER_BUNDLE_ID:-$(git config hyper.bundleId 2>/dev/null || echo io.github.bashkev.hyper)}
name=Hyper id=$bundle_id open= reload=
for arg in "$@"; do
  case $arg in
    --dev) name="Hyper Dev" id="$bundle_id.dev" ;;
    --open) open=1 ;;
    --reload) reload=1 ;;
    *) echo "usage: $0 [--dev] [--open | --reload]" >&2; exit 2 ;;
  esac
done

cargo build --release

app="target/$name.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/hyper "$app/Contents/MacOS/Hyper"

cp assets/Hyper.icns assets/Assets.car "$app/Contents/Resources/"
# Agents started in Hyper are pointed at it (src/prompt.rs).
cp README.md "$app/Contents/Resources/"

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>Hyper</string>
  <key>CFBundleIdentifier</key><string>$id</string>
  <key>CFBundleName</key><string>$name</string>
  <key>CFBundleIconFile</key><string>Hyper</string>
  <key>CFBundleIconName</key><string>Hyper</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>NSHighResolutionCapable</key><true/>
  <!-- The browser pane opens any page, including local dev servers over HTTP. -->
  <key>NSAppTransportSecurity</key><dict><key>NSAllowsArbitraryLoadsInWebContent</key><true/></dict>
</dict>
</plist>
PLIST

# macOS keeps the Accessibility and Screen Recording that agents in Hyper need for a signing
# identity across builds, but for an ad-hoc signature only until the next build.
# HYPER_SIGN_IDENTITY picks one; otherwise the first Apple Development identity, if any.
identity=${HYPER_SIGN_IDENTITY:-$(security find-identity -v -p codesigning 2>/dev/null | awk '/"Apple Development/ { print $2; exit }')}
# Without --timestamp=none, an Apple identity signs over the network, and fails offline.
if ! codesign --sign "${identity:--}" --timestamp=none --force "$app" > /dev/null 2>&1; then
  echo "Couldn't sign with $identity; signed ad hoc, so macOS forgets permissions on each build." >&2
  codesign --sign - --force "$app" > /dev/null 2>&1
fi
echo "$app"

if [ -n "$reload" ]; then
  # -a: run from a terminal inside that Hyper, the app is an ancestor, which pgrep skips otherwise.
  running=$(pgrep -a -f "$PWD/$app/Contents/MacOS/Hyper" || true)
  [ -z "$running" ] && open=1
  for pid in $running; do
    # To a copy built before reloading existed, the signal would be fatal: it says it reloads.
    # A copy writes its marker once started, so one from before it started is another's, whose
    # pid it reuses.
    marker="${TMPDIR:-/tmp}/hyper-reloads/$pid"
    started=$(LC_ALL=C date -j -f "%a %b %d %T %Y" "$(LC_ALL=C ps -o lstart= -p "$pid")" +%s 2>/dev/null || echo 0)
    if [ -e "$marker" ] && [ "$(stat -f %m "$marker")" -ge "$started" ] && [ "$started" -gt 0 ]; then
      kill -USR1 "$pid"
    else
      echo "$app ($pid) can't reload: it predates reloading. Quit it and use --open." >&2
      exit 1
    fi
  done
fi

if [ -n "$open" ]; then
  # `open` would only bring a running copy forward, still on its old code.
  pkill -f "$PWD/$app/Contents/MacOS/Hyper" || true
  open -n "$app"
fi
