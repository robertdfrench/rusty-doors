use doors::illumos::DoorArg;
use doors::Client;

#[test]
fn procmac_increment_shared_counter() {
    let increment = Client::open("/tmp/procmac_kv_store.door").unwrap();
    let fetch = Client::open("/tmp/procmac_kv_fetch.door").unwrap();

    let mut rbuf: [u8; 1] = [0];

    let mut arg = DoorArg::new(&[], &[], &mut rbuf);
    increment.call(arg.as_mut_door_arg_t()).unwrap();
    increment.call(arg.as_mut_door_arg_t()).unwrap();
    increment.call(arg.as_mut_door_arg_t()).unwrap();
    increment.call(arg.as_mut_door_arg_t()).unwrap();
    fetch.call(arg.as_mut_door_arg_t()).unwrap();
    assert_eq!(rbuf[0], 4);
}
