#!/bin/bash
cargo build || exit $?
sudo setcap 'cap_net_bind_service+eip cap_sys_nice+eip' ./target/debug/aes67-vsc-rtp || exit $?
sudo setcap 'cap_net_bind_service+eip cap_sys_nice+eip' ./target/debug/aes67-vsc-ptp || exit $?
sudo setcap 'cap_net_bind_service+eip cap_sys_nice+eip' ./target/debug/aes67-vsc-jack || exit $?
# ./target/debug/aes67-vsc-jack "$@"
