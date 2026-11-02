#!/bin/sh
# Stretches the ms-timed (menu) items of a recording 2.5 times, for runs that slow the menus
# (GUNMETAL_COUNT); mission items (t<N>:) stay. usage: scripts/stretch.sh in.txt > out.txt
awk -F: '/^[0-9]/ { printf "%d:%s\n", $1 * 2.5, $2; next } { print }' "$1"
