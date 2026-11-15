#!/bin/sh
# usage: score32.sh v1 ... v32   (integers 0..255). Lower is better, 0 is perfect.
[ "$#" -eq 32 ] || { echo "need exactly 32 integers" >&2; exit 2; }
echo "$@" | awk '{ st=0; sx=0; st2=0
  for (i=1;i<=32;i++) { x=$i; if (x !~ /^[0-9]+$/ || x<0 || x>255) { print "bad value" > "/dev/stderr"; exit 2 }
    t=(i*53+29)%256; d=x-t; st+=d*d; sx+=x; st2+=t }
  e=sx-st2; if (e<0) e=-e; print st + 2*e }'
