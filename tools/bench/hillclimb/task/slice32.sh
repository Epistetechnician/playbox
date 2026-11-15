#!/bin/sh
# usage: slice32.sh K v... ; scores ONLY slice K of the 32-vector (K=1: coords 1-11, 2: 12-22, 3: 23-32). Squared distance to the hidden target on that slice.
k=$1; shift
case $k in 1) a=1; n=11;; 2) a=12; n=11;; 3) a=23; n=10;; *) echo "bad slice" >&2; exit 2;; esac
[ "$#" -eq "$n" ] || { echo "slice $k needs exactly $n integers" >&2; exit 2; }
echo "$@" | awk -v a=$a -v n=$n '{ st=0
  for (j=1;j<=n;j++) { x=$j; if (x !~ /^[0-9]+$/ || x<0 || x>255) { print "bad value" > "/dev/stderr"; exit 2 }
    i=a+j-1; t=(i*53+29)%256; d=x-t; st+=d*d }
  print st }'
