# The release build environment (M1.8): Ubuntu 24.04, the target's own release, building the amd64 binary
# for the target box by cross-compiling at native speed on the arm64 Colima VM. (An amd64 container under QEMU
# was tried first: gcc's cc1 segfaulted compiling sqlite and lzma there.) The amd64 libc and libasound come
# from Ubuntu's own packages, so the binary links against exactly what Ubuntu Server 24.04 has.
#   docker build -t hotclock-release -f dev/release.Dockerfile dev       (dev/package.sh does it)
FROM ubuntu:24.04
ENV DEBIAN_FRONTEND=noninteractive RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:$PATH
# amd64 packages beside the native arm64 ones: arm64 from ports.ubuntu.com (the default), amd64 from
# archive.ubuntu.com.
RUN dpkg --add-architecture amd64 \
 && sed -i 's/^Types: deb$/Types: deb\nArchitectures: arm64/' /etc/apt/sources.list.d/ubuntu.sources \
 && printf 'Types: deb\nURIs: http://archive.ubuntu.com/ubuntu/\nSuites: noble noble-updates noble-security\nComponents: main universe\nArchitectures: amd64\nSigned-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg\n' > /etc/apt/sources.list.d/amd64.sources \
 && apt-get -qq update \
 && apt-get -qq install -y --no-install-recommends \
      build-essential pkg-config curl ca-certificates git xz-utils file binutils \
      dpkg-dev fakeroot lintian \
      gcc-x86-64-linux-gnu libc6-dev-amd64-cross libasound2-dev libasound2-dev:amd64 \
 && rm -rf /var/lib/apt/lists/*
# The same toolchain as the Mac and the Linux test container, with the amd64 target.
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain 1.98.1 \
      --target x86_64-unknown-linux-gnu \
 && rustup component add clippy rustfmt
RUN cargo install cargo-deb --locked --version '^3'
ENV CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc \
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc \
    PKG_CONFIG_ALLOW_CROSS=1 \
    PKG_CONFIG_PATH_x86_64_unknown_linux_gnu=/usr/lib/x86_64-linux-gnu/pkgconfig \
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-C link-arg=-L/usr/lib/x86_64-linux-gnu"
WORKDIR /src
