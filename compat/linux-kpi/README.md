# Omit OS LinuxKPI

A compatibility layer that lets selected, unmodified Linux kernel drivers be
compiled into the Omit OS kernel. It is **not** Linux and contains no Linux
code: the headers in `include/linux/` are written for Omit OS and declare
the same functions and structures a driver expects, and those functions are
implemented in Rust (`kernel/src/linuxkpi/`) on top of native Omit OS
facilities.

Linux has no stable in-kernel driver API, so compatibility is per driver and
per Linux version: each ported driver is listed in
[`drivers/linux/README.md`](../../drivers/linux/README.md) with the exact
upstream revision it was taken from, and the interfaces provided here are
exactly the ones those drivers use.

Drivers are linked statically. There are no loadable modules, and module
parameters keep their compiled-in defaults.
