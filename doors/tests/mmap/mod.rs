use doors::Client;
use doors::DoorArgument;

#[test]
fn can_own_rbuf() {
    let junk = Client::open("/tmp/junk.door").unwrap();

    let mut rbuf: [u8; 1] = [0];
    let arg = DoorArgument::new(&[111], &[], &mut rbuf);

    let owned = match junk.call(arg).unwrap() {
        DoorArgument::OwnedRbuf(_) => true,
        DoorArgument::BorrowedRbuf(_) => false,
    };
    assert!(owned);
}

#[test]
fn can_munmap() {
    let junk = Client::open("/tmp/junk.door").unwrap();

    let mut rbuf: [u8; 1] = [0];
    let arg = DoorArgument::new(&[111], &[], &mut rbuf);

    let response = junk.call(arg).unwrap();

    drop(response);
    assert!(true); // Assert that dropping response doesn't panic
}

#[test]
fn owned_rbuf_is_expected_length() {
    let junk = Client::open("/tmp/junk.door").unwrap();

    let mut rbuf: [u8; 1] = [0];
    let arg = DoorArgument::new(&[111], &[], &mut rbuf);
    assert_eq!(arg.rbuf().len(), 1);

    let response = junk.call(arg).unwrap();
    assert_eq!(response.rbuf().len(), 4096);
}

#[test]
fn can_borrow_rbuf() {
    let no_junk = Client::open("/tmp/no_junk.door").unwrap();

    let mut rbuf: [u8; 1] = [0];
    let arg = DoorArgument::new(&[111], &[], &mut rbuf);

    let borrowed = match no_junk.call(arg).unwrap() {
        DoorArgument::OwnedRbuf(_) => false,
        DoorArgument::BorrowedRbuf(_) => true,
    };
    assert!(borrowed);
}

#[test]
fn borrowed_rbuf_is_expected_length() {
    let no_junk = Client::open("/tmp/no_junk.door").unwrap();

    let mut rbuf: [u8; 1] = [0];
    let arg = DoorArgument::new(&[111], &[], &mut rbuf);

    let response = no_junk.call(arg).unwrap();
    assert_eq!(response.rbuf().len(), 1);
}
