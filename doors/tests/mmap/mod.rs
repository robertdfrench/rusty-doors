use doors::Client;
use doors::DoorArgument;

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
