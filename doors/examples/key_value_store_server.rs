//! A Barebones server using only the illumos headers, and no additional
//! support. This helps validate that the headers are expressed correctly in
//! Rust.

use doors::illumos::DoorAttributes;
use doors::server;
use doors::server::ServerProcedure;
use std::sync::atomic::{AtomicU8, Ordering};

static mut COUNT: AtomicU8 = AtomicU8::new(0);

struct Increment {}

impl<'a> ServerProcedure<&'a [u8]> for Increment {
    fn server_procedure(
        _payload: server::Request<'_>,
    ) -> server::Response<&'a [u8]> {
        unsafe { COUNT.fetch_add(1, Ordering::SeqCst) };

        server::Response::empty()
    }
}

struct Fetch {}

impl ServerProcedure<[u8; 1]> for Fetch {
    fn server_procedure(
        _payload: server::Request<'_>,
    ) -> server::Response<[u8; 1]> {
        let x = unsafe { COUNT.load(Ordering::SeqCst) };

        server::Response::new([x])
    }
}

fn main() {
    let increment =
        Increment::create_server_with_attributes(DoorAttributes::refuse_desc())
            .unwrap();
    std::fs::remove_file("/tmp/key_value_store_server.door").unwrap();
    increment
        .install("/tmp/key_value_store_server.door")
        .unwrap();

    let fetch = Fetch::create_server_with_attributes(
        DoorAttributes::refuse_desc() | DoorAttributes::unref_multi(),
    )
    .unwrap();
    std::fs::remove_file("/tmp/key_value_store_server_fetch.door").unwrap();
    fetch
        .install("/tmp/key_value_store_server_fetch.door")
        .unwrap();

    std::thread::sleep(std::time::Duration::from_secs(5));
}
