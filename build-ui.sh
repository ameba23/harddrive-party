#!/bin/sh

# Default build type is release
BUILD_ARGS="--release"

# Check for --debug flag
for arg in "$@"; do
  if [ "$arg" = "--debug" ]; then
    BUILD_ARGS=""
    break
  fi
done

cd web-ui
trunk build $BUILD_ARGS --dist ../dist
cd ..
