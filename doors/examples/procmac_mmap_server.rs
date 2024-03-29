//! Return a ton of data that will need to be mmap'd into the caller's address
//! space.

use doors::server::{Door, Request, Response};

#[doors::server_procedure]
fn return_junk(_payload: Request<'_>) -> Response<[u8; 4096]> {
    let mut x: [u8; 4096] = [0; 4096];
    for i in 0..4096 {
        x[i] = (i % 255) as u8;
    }
    Response::new(x)
}

#[doors::server_procedure]
fn return_no_junk(_payload: Request<'_>) -> Response<[u8; 1]> {
    let x: [u8; 1] = [6; 1];
    Response::new(x)
}

fn main() {
    let door = Door::create(return_junk).unwrap();
    door.force_install("/tmp/junk.door").unwrap();

    let door2 = Door::create(return_no_junk).unwrap();
    door2.force_install("/tmp/no_junk.door").unwrap();

    std::thread::sleep(std::time::Duration::from_secs(5));
}
