default: build-all

build-all: build-linux # build-macos build-windows

build-linux: build-linux-x86_64 build-linux-i686 # build-linux-arm64 build-linux-armv7

build-linux-x86_64:
    cross build --release --target=x86_64-unknown-linux-musl

build-linux-i686:
    cross build --release --target=i686-unknown-linux-musl

# build-linux-arm64:
#     cross build --release --target=aarch64-unknown-linux-musl

# build-linux-armv7:
#     cross build --release --target=arm-unknown-linux-musleabihf

# build-windows: # build-windows-x86_64 build-windows-arm64

# build-windows-x86_64:
#     cross build --release --target=x86_64-pc-windows-musl

# build-windows-arm64:
#     cross build --release --target=aarch64-pc-windows-msvc

# build-macos: build-macos-x86_64 build-macos-arm64

# build-macos-x86_64:
#     cross build --release --target=x86_64-apple-darwin

# build-macos-arm64:
#     cross build --release --target=aarch64-apple-darwin

clean:
    cross clean


dev:
  RUST_LOG="wallhack=trace,wallhack::host=trace,wallhack::agent=trace" \
    cargo run --bin host -- \
        -t noclip0 \
        connect localhost:6565
