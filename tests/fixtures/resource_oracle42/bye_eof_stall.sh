#!/bin/sh
# Negative transport fixture: valid empty BYE/EOF, then a still-live direct child.
"$1" "$2" bye_only || exit 11
exec 1>&-
exec sleep 10
