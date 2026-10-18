#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Gianluca Boiano
# SPDX-License-Identifier: GPL-3.0-only
#
# Adds the Java side (classes.dex + its manifest <service>) to the APK that
# `cargo apk build` just produced, then aligns and signs it again.
#
#   cargo apk build [--release] -p aulos --lib [--features visualizer]
#   crates/aulos/android/package.sh debug|release
#
# Why: cargo-apk's manifest model has no <service>/<receiver> and cannot add a
# classes.dex. It leaves the generated AndroidManifest.xml and the lib/<abi>
# tree in target/<profile>/apk, so this script repeats cargo-apk's own last
# steps (aapt package, aapt add, zipalign, apksigner) on those files with the
# <service> inserted and classes.dex added. The output replaces
# target/<profile>/apk/aulos.apk, signed with the same key cargo-apk used:
#   release: $CARGO_APK_RELEASE_KEYSTORE + $CARGO_APK_RELEASE_KEYSTORE_PASSWORD
#   debug:   ~/.android/debug.keystore (password "android"), made by cargo-apk.
# Needs ANDROID_HOME (or ANDROID_SDK_ROOT) with build-tools and platform 34.
set -euo pipefail

profile=${1:?usage: package.sh debug|release}
case "$profile" in
  debug | release) ;;
  *) echo "profile must be debug or release" >&2; exit 2 ;;
esac

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../.." && pwd)
dir=${CARGO_TARGET_DIR:-$root/target}/$profile/apk
sdk=${ANDROID_HOME:-${ANDROID_SDK_ROOT:?set ANDROID_HOME}}
apk_name=aulos
target_sdk=34

# cargo-apk uses the greatest build-tools directory name.
# shellcheck disable=SC2012
tools="$sdk/build-tools/$(ls -1 "$sdk/build-tools" | LC_ALL=C sort | tail -1)"
android_jar="$sdk/platforms/android-$target_sdk/android.jar"

[ -f "$dir/AndroidManifest.xml" ] || {
  echo "no $dir/AndroidManifest.xml: run cargo apk build first" >&2
  exit 1
}
[ -f "$here/classes.dex" ] || { echo "missing $here/classes.dex" >&2; exit 1; }

cd "$dir"
cp "$here/classes.dex" classes.dex

# The foreground service. `foregroundServiceType` is mandatory on Android 14
# (target 34) and what the dex's `startForeground(id, notification)` relies on.
service='<service android:name="io.github.m0rf30.aulos.MediaService" android:exported="false" android:foregroundServiceType="mediaPlayback"/>'
if ! grep -q 'io.github.m0rf30.aulos.MediaService' AndroidManifest.xml; then
  grep -q '</application>' AndroidManifest.xml
  # `&` in a sed replacement would be special; the service string has none.
  sed -i "s#</application>#${service}</application>#" AndroidManifest.xml
fi

# cargo-apk stores the debug build uncompressed.
compression=()
if [ "$profile" = debug ]; then
  compression=(-0 "")
fi

unaligned="$apk_name-unaligned.apk"
rm -f "$unaligned"
"$tools/aapt" package -f -F "$unaligned" -M AndroidManifest.xml -I "$android_jar" \
  -S "$root/crates/aulos/android/res" -A "$root/crates/aulos-core/assets" \
  ${compression[@]+"${compression[@]}"}
# Every lib/<abi>/*.so cargo-apk copied, plus the dex.
mapfile -t files < <(find lib -type f -name '*.so' | LC_ALL=C sort)
"$tools/aapt" add ${compression[@]+"${compression[@]}"} "$unaligned" "${files[@]}" classes.dex
"$tools/zipalign" -f -v 4 "$unaligned" "$apk_name.apk" > /dev/null

if [ "$profile" = release ]; then
  : "${CARGO_APK_RELEASE_KEYSTORE:?CARGO_APK_RELEASE_KEYSTORE is not set}"
  : "${CARGO_APK_RELEASE_KEYSTORE_PASSWORD:?CARGO_APK_RELEASE_KEYSTORE_PASSWORD is not set}"
  keystore=$CARGO_APK_RELEASE_KEYSTORE
  export APK_KEYSTORE_PASSWORD=$CARGO_APK_RELEASE_KEYSTORE_PASSWORD
else
  keystore=${HOME}/.android/debug.keystore
  export APK_KEYSTORE_PASSWORD=android
fi
"$tools/apksigner" sign --ks "$keystore" --ks-pass env:APK_KEYSTORE_PASSWORD "$apk_name.apk"
"$tools/apksigner" verify "$apk_name.apk"
echo "packaged $dir/$apk_name.apk"
