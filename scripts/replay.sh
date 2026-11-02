#!/bin/sh
# Replays a recording and prints ms per picture (pictures without a tick) in two time windows.
# usage: scripts/replay.sh <run name> [VAR=value ...]
#   REC=look (default): look.txt, the base 44-56 s against looking away 58-72 s
#   REC=anim: anim.txt, the battle 40-49 s against quiet 53-62 s
# Deletes the savegames first.
cd "$(dirname "$0")/.."
name=$1; shift
rec=${REC:-look}
case $rec in
  anim) a="40 49"; b="53 62" ;;
  *) a="44 56"; b="58 72" ;;
esac
rm -f game_files/data/SavedGames/*
env "$@" GUNMETAL_INPUT=@$rec.txt GUNMETAL_FRAMES=$name.frames timeout 200 target/release/gunmetal.exe 2> $name.log
awk -v n=$name -v a="$a" -v b="$b" 'BEGIN { split(a, A, " "); split(b, B, " ") }
  !/^#/ && $7==1 && $5==0 { t=$1/1000; if (t>=A[1]&&t<A[2]) {x+=$3;nx++} if (t>=B[1]&&t<B[2]) {y+=$3;ny++} }
  END { printf "%s: A %.2f ms (%d pictures)  B %.2f ms (%d)\n", n, nx?x/nx:0, nx, ny?y/ny:0, ny }' $name.frames
