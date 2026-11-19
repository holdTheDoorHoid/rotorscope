#!/usr/bin/env bash
# Build, preview or check the Rotorscope documentation site.
#
#   tools/docs-build.sh          build into target/book
#   tools/docs-build.sh serve    live preview at http://localhost:3000
#   tools/docs-build.sh check    build, then look for broken links and
#                                pages missing from the sidebar
#
# Needs mdBook (cargo install mdbook --version 0.5.4 --locked).
# See docs/dev/docs-style.md.
set -euo pipefail

cd "$(dirname "$0")/.."

MERMAID_VERSION=11.4.1
MERMAID_SHA256=a43bc1afd446f9c4cc66ac5dd45d02e8d65e26fc5344ec0ef787f88d6ddb6f9e
MERMAID=docs/assets/mermaid.min.js

command -v mdbook >/dev/null || {
    echo "mdbook not found. Install it with:" >&2
    echo "  cargo install mdbook --version 0.5.4 --locked" >&2
    exit 1
}

# The diagram library is fetched once and checked, not committed (2.5 MB).
have_mermaid() {
    [ -f "$MERMAID" ] && echo "$MERMAID_SHA256  $MERMAID" | sha256sum --check --status
}
if ! have_mermaid; then
    echo "Fetching Mermaid $MERMAID_VERSION (diagrams)..."
    curl -fsSL --proto '=https' --proto-redir '=https' -o "$MERMAID" \
        "https://cdn.jsdelivr.net/npm/mermaid@${MERMAID_VERSION}/dist/mermaid.min.js"
    have_mermaid || {
        rm -f "$MERMAID"
        echo "Mermaid download did not match its checksum; refusing to use it." >&2
        exit 1
    }
fi

# The screen pictures are copies of the renderer's own test snapshots, so the
# docs always show what the code draws.
mkdir -p docs/images/screen
cp crates/screen/snapshots/*.png docs/images/screen/

# The P4 hub's 800x480 layout: every picture in the default theme (Glass
# Cockpit), and a few in the other two themes for the pages that compare them.
mkdir -p docs/images/screen/wide
cp crates/screen/snapshots/wide/glass-cockpit/*.png docs/images/screen/wide/
for theme in field-day red-night; do
    mkdir -p "docs/images/screen/wide/$theme"
    for shot in radar_5_drones list_5_drones detail band_many_warnings settings_brightness_capped; do
        cp "crates/screen/snapshots/wide/$theme/$shot.png" "docs/images/screen/wide/$theme/"
    done
done

case "${1:-build}" in
    build)
        mdbook build
        echo "Built: target/book/index.html"
        ;;
    serve)
        shift
        mdbook serve "$@"
        ;;
    check)
        mdbook build
        python3 tools/docs-check.py
        ;;
    *)
        echo "usage: tools/docs-build.sh [build|serve|check]" >&2
        exit 2
        ;;
esac
