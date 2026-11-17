#!/bin/sh
# Rebuild every shipped logo asset from brand/*.svg (run after brand/trace.py). Needs rsvg-convert.
set -e
cd "$(dirname "$0")/.."
D=ui/static/brand; mkdir -p $D
cp brand/icon.svg brand/mark.svg brand/logo.svg $D/
rsvg-convert -w 192 brand/icon.svg -o $D/icon-192.png
rsvg-convert -w 512 brand/icon.svg -o $D/icon-512.png
rsvg-convert -w 512 brand/icon-maskable.svg -o $D/icon-maskable.png
cp $D/icon-512.png claude-plugin/.claude-plugin/icon.png
cp brand/mark.svg site/mark.svg
cp brand/icon.svg site/favicon.svg
