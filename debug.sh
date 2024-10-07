#!/bin/bash
cargo build || exit $?
sudo setcap cap_sys_nice+eip ./target/debug/aes67-vsc || exit $?
sudo setcap cap_net_bind_service=ep ./target/debug/aes67-vsc || exit $?
./target/debug/aes67-vsc "$@"
