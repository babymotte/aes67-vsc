#!/bin/bash
cargo build || exit $?
sudo sudo setcap CAP_SYS_NICE+eip ./target/debug/aes67-vsc || exit $?
sudo setcap CAP_NET_BIND_SERVICE=ep ./target/debug/aes67-vsc || exit $?
./target/debug/aes67-vsc "$@"
