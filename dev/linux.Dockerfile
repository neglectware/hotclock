# Linux build and test environment for hotclock (milestone M1.1 onward).
#   docker build -t hotclock-dev -f dev/linux.Dockerfile dev
#   dev/linux-test.sh            # runs `cargo test --workspace` in it
# Debian bookworm, like the target's userspace family; ALSA headers for the `alsa` crate, ffmpeg
# for the fixture generator (FLAC, MP3, M4A encoding), tzdata for later milestones.
FROM rust:1-bookworm
RUN apt-get -qq update \
 && DEBIAN_FRONTEND=noninteractive apt-get -qq install -y --no-install-recommends \
      libasound2-dev pkg-config ffmpeg flac tzdata alsa-utils \
 && rm -rf /var/lib/apt/lists/*
RUN rustup component add clippy rustfmt
WORKDIR /src
