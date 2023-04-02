//! A Barebones server using only the illumos headers, and no additional
//! support. This helps validate that the headers are expressed correctly in
//! Rust.

use doors::server::ServerProcedure;
use libc;
use std::fmt;
use std::sync::atomic::{AtomicU16, Ordering};

extern "C" {
    pub fn thr_self() -> libc::c_uint;
}

struct KnockCounter(AtomicU16);

impl KnockCounter {
    const fn new() -> Self {
        Self(AtomicU16::new(0))
    }

    fn increment(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

impl ServerProcedure for KnockCounter {
    fn server_procedure(&mut self) {
        self.increment();
    }
}

impl fmt::Display for KnockCounter {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0.load(Ordering::SeqCst))
    }
}

static mut KC: KnockCounter = KnockCounter::new();

fn main() {
    let door_path = std::path::Path::new("/tmp/knock_only_server.door");
    if door_path.exists() {
        std::fs::remove_file(door_path).unwrap();
    }
    unsafe { KC.install("/tmp/knock_only_server.door", 0) }
        .expect("install door");
    std::thread::sleep(std::time::Duration::from_secs(5));
    unsafe { println!("There were {} knocks", KC) };
}
