#!/bin/bash
# Validates that Cargo.toml version matches a git tag
set -e

TAG_VERSION="${1#v}"  # Strip leading 'v' if present

if [ -z "$TAG_VERSION" ]; then
    echo "Usage: $0 <tag-version>"
    echo "Example: $0 v1.0.2"
    exit 1
fi

CARGO_VERSION=$(grep '^version = ' Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/')

if [ "$CARGO_VERSION" != "$TAG_VERSION" ]; then
    echo "ERROR: Cargo.toml version ($CARGO_VERSION) does not match tag version ($TAG_VERSION)"
    echo ""
    echo "Please update Cargo.toml version to $TAG_VERSION before tagging."
    exit 1
fi

echo "Version check passed: $CARGO_VERSION"
