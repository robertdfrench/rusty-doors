use doors::client;
use doors::illumos::door_h;

#[test]
fn procmac_increment_shared_counter() {
    let increment = client::Client::open("/tmp/procmac_kv_store.door").unwrap();
    let fetch = client::Client::open("/tmp/procmac_kv_fetch.door").unwrap();

    let mut rbuf: [u8; 1] = [0];

    let mut arg = door_h::door_arg_t::new(&[], &[], &mut rbuf);
    increment.call(&mut arg).unwrap();
    increment.call(&mut arg).unwrap();
    increment.call(&mut arg).unwrap();
    increment.call(&mut arg).unwrap();
    fetch.call(&mut arg).unwrap();
    assert_eq!(rbuf[0], 4);
}
