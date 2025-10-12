#!/usr/bin/env bash
# shellcheck shell=bash

_system_lib_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../../lib/systemd.sh
source "${_system_lib_dir}/../../lib/systemd.sh"
unset _system_lib_dir
