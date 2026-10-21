#!/bin/sh
# Rebuild the checked-in offline runtime after editing viewer.js or scene.js.
set -eu
cd "$(dirname "$0")/.."
npm exec --yes --package=esbuild@0.25.5 -- esbuild admin/wiring/viewer.js \
  --bundle --minify --format=iife --legal-comments=inline \
  --outfile=admin/wiring/viewer.bundle.js
