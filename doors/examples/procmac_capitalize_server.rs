//! A Barebones server using only the illumos headers, and no additional
//! support. This helps validate that the headers are expressed correctly in
//! Rust.

use doors::server::{Door, Request, Response};

#[doors::server_procedure]
fn double(payload: Request<'_>) -> Response<[u8; 1]> {
    Response::new([payload.data[0] * 2])
}

fn main() {
    let door = Door::create(double).unwrap();
    door.force_install("/tmp/procmac_double.door").unwrap();

    std::thread::sleep(std::time::Duration::from_secs(5));
}
