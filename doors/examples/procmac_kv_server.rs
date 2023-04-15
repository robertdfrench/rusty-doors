//! A Barebones server using only the illumos headers, and no additional
//! support. This helps validate that the headers are expressed correctly in
//! Rust.

use doors::server::{Door, Request, Response};
use std::sync::atomic::{AtomicU8, Ordering};

static mut COUNT: AtomicU8 = AtomicU8::new(0);

#[doors::server_procedure]
fn increment(_payload: Request<'_>) -> Response<[u8; 0]> {
    unsafe { COUNT.fetch_add(1, Ordering::SeqCst) };
    Response::empty()
}

#[doors::server_procedure]
fn fetch(_payload: Request<'_>) -> Response<[u8; 1]> {
    let x = unsafe { COUNT.load(Ordering::SeqCst) };
    Response::new([x])
}

fn main() {
    let increment_door = Door::create(increment).unwrap();
    increment_door
        .force_install("/tmp/procmac_kv_store.door")
        .unwrap();

    let fetch_door = Door::create(fetch).unwrap();
    fetch_door
        .force_install("/tmp/procmac_kv_fetch.door")
        .unwrap();

    std::thread::sleep(std::time::Duration::from_secs(5));
}
