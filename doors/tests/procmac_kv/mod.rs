use doors::Client;
use doors::DoorArgument;

#[test]
fn procmac_increment_shared_counter() {
    let increment = Client::open("/tmp/procmac_kv_store.door").unwrap();
    let fetch = Client::open("/tmp/procmac_kv_fetch.door").unwrap();

    let mut rbuf: [u8; 1] = [0];

    let arg = DoorArgument::new(&[], &[], &mut rbuf);
    increment.call(arg).unwrap();
    let arg = DoorArgument::new(&[], &[], &mut rbuf);
    increment.call(arg).unwrap();
    let arg = DoorArgument::new(&[], &[], &mut rbuf);
    increment.call(arg).unwrap();
    let arg = DoorArgument::new(&[], &[], &mut rbuf);
    increment.call(arg).unwrap();
    let arg = DoorArgument::new(&[], &[], &mut rbuf);
    fetch.call(arg).unwrap();
    assert_eq!(rbuf[0], 4);
}
