# Name of the application's binary.
name := 'lyra'
# The unique ID of the application.
appid := 'io.github.m0rf30.Lyra'

# Path to root file system, which defaults to `/`.
rootdir := ''
# The prefix for the `/usr` directory.
prefix := '/usr'
# The location of the cargo target directory.
cargo-target-dir := env('CARGO_TARGET_DIR', 'target')
# Binary to install. Defaults to a `lyra` binary next to the justfile (as shipped
# in release tarballs), falling back to the cargo release build. Override with
# `just bin-src=/path/to/lyra install`.
bin-src := if path_exists(justfile_directory() / name) == 'true' { justfile_directory() / name } else { cargo-target-dir / 'release' / name }

# Application's appstream metadata
appdata := appid + '.metainfo.xml'
# Application's desktop entry
desktop := appid + '.desktop'

# Install destinations
base-dir := absolute_path(clean(rootdir / prefix))
appdata-dst := base-dir / 'share' / 'appdata' / appdata
bin-dst := base-dir / 'bin' / name
desktop-dst := base-dir / 'share' / 'applications' / desktop
icons-dst := base-dir / 'share' / 'icons' / 'hicolor'
icon-svg-dst := icons-dst / 'scalable' / 'apps'

# Default recipe which runs `just build-release`
default: build-release

# Runs `cargo clean`
clean:
    cargo clean

# Removes vendored dependencies
clean-vendor:
    rm -rf .cargo vendor vendor.tar

# `cargo clean` and removes vendored dependencies
clean-dist: clean clean-vendor

# Compiles with debug profile
build-debug *args:
    cargo build {{args}}

# Compiles with release profile
build-release *args: (build-debug '--release' args)

# Compiles release profile with vendored dependencies
build-vendored *args: vendor-extract (build-release '--frozen --offline' args)

# Runs a clippy check
check *args:
    cargo clippy --all-features {{args}} -- -W clippy::pedantic

# Runs a clippy check with JSON message format
check-json: (check '--message-format=json')

# Run the application for testing purposes
run *args:
    env RUST_BACKTRACE=full cargo run --release {{args}}

# Installs files
install:
    install -Dm0755 {{bin-src}} {{bin-dst}}
    install -Dm0644 {{ 'resources' / desktop }} {{desktop-dst}}
    install -Dm0644 {{ 'resources' / appdata }} {{appdata-dst}}
    install -Dm0644 resources/icons/hicolor/scalable/apps/{{appid}}.svg {{icon-svg-dst}}/{{appid}}.svg

# Uninstalls installed files
uninstall:
    rm -f {{bin-dst}} {{desktop-dst}} {{icon-svg-dst}}/{{appid}}.svg

# Vendor dependencies locally
vendor:
    mkdir -p .cargo
    cargo vendor | head -n -1 > .cargo/config.toml
    echo 'directory = "vendor"' >> .cargo/config.toml
    tar pcf vendor.tar vendor
    rm -rf vendor

# Extracts vendored dependencies
vendor-extract:
    rm -rf vendor
    tar pxf vendor.tar
