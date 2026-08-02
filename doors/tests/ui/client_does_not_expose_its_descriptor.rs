// GOALS.md §12.6, the client half.
use std::os::fd::AsRawFd;

fn main() {}

fn leak(client: &doors::Client) -> std::os::fd::RawFd {
    client.as_raw_fd()
}
