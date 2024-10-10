#!/bin/bash
cargo install --path . || exit $?
sudo setcap 'cap_net_bind_service+eip cap_sys_nice+eip' $(which aes67-vsc-rtp) || exit $?
sudo setcap 'cap_net_bind_service+eip cap_sys_nice+eip' $(which aes67-vsc-jack) || exit $?
