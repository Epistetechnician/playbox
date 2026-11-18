#!/bin/sh
# 15 P-arm runs, one at a time (so wall-clock is not distorted). PLAYBOX_BIN must point at a playbox with the large-workspace upload fix.
cd "$(dirname "$0")" || exit 1
for spec in "django-11848 1 2 3 4 5" "django-11422 1 2 3 4 5" "django-11905 1 2 3 4 5"; do
  set -- $spec; inst=$1; shift
  for n in "$@"; do PB_MAX_RUNS=${PB_MAX_RUNS:-200} python3 run_swe_playbox.py "$inst" "$n" swe-results-playbox; done
done
