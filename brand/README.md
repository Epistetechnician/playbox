# Brand assets

`logo.svg` (full colour), `mark.svg` (single colour, uses `currentColor`/CSS mask), `icon.svg` / `icon-maskable.svg` (app tiles).
They are traced from the delivered logo, which was a raster mask wrapped in an SVG: `python3 -I brand/trace.py <3.svg>`, then `brand/build.sh` copies/renders them into `ui/static/brand/`, `site/` and the Claude plugin icon.
