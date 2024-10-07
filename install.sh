#!/bin/bash
cargo install --path . || exit $?
sudo setcap cap_sys_nice+eip $(which aes67-vsc) || exit $?
sudo setcap cap_net_bind_service=ep $(which aes67-vsc) || exit $?
