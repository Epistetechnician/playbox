#!/bin/sh
# usage: score.sh v1 v2 ... v16   (integers 0..255). Prints the score; lower is better, 0 is perfect.
[ "$#" -eq 16 ] || { echo "need exactly 16 integers" >&2; exit 2; }
echo "$@" | awk '{ st=0; sx=0; st2=0
  for (i=1;i<=16;i++) { x=$i; if (x !~ /^[0-9]+$/ || x<0 || x>255) { print "bad value" > "/dev/stderr"; exit 2 }
    t=(i*37+11)%256; d=x-t; st+=d*d; sx+=x; st2+=t }
  e=sx-st2; if (e<0) e=-e; print st + 2*e }'
