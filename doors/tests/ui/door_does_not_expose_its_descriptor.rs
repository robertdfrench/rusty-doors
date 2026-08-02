// GOALS.md §12.6: Door never exposes its raw file descriptor. Handing
// it out would let someone close it while the fork registry still
// believed it was open.
use std::os::fd::AsRawFd;

fn main() {}

fn leak(door: &doors::Door<String>) -> std::os::fd::RawFd {
    door.as_raw_fd()
}
