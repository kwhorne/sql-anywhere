#!/usr/bin/env bash

# Generates artifacts from the current build:
# - .c and .h amalgamation files
# - a precompiled binary package
#
# Assumes that ./configure and make steps were executed and succeeded

set -x

tar czvf sqlanywhere-amalgamation-$(<SQLANYWHERE_VERSION)${SQLANYWHERE_SUFFIX}.tar.gz sqlite3.c sqlite3.h
tar czvf sqlanywhere-$(<SQLANYWHERE_VERSION)${SQLANYWHERE_SUFFIX}.tar.gz sqlite3 sqlanywhere .libs
