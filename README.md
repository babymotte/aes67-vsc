# AES67 Virtual Sound Card

A virtual sound card for Linux written in Rust

This is a very experimental project in its early phase, please do not attempt to use this for any kind of productive work.

Goals for this project:
- [ ] AES67 compatible sender(s) with a configurable number of channels
- [ ] AES67 compatible receiver(s) with a configurable number of channels
- [ ] make the VSC available as an ALSA audio device for I/O on desktop computers
- [x] publish active sessions over SAP
- [ ] NMOS IS-04 and IS-05 support
- [ ] interoperability with Dante
- [ ] status monitoring over Wörterbuch