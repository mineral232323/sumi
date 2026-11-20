#!/bin/sh
set -eu

cd "$(dirname "$0")"

echo "==> git pull"
git pull --ff-only

echo "==> cargo build --release"
cargo build --release

echo "==> version"
./target/release/sumi --version

echo "==> install.sh"
./install.sh
