use doors::client;
use doors::illumos::door_h;

#[test]
fn procedural_macro_double_u8() {
    let double = client::Client::open("/tmp/procmac_double.door").unwrap();

    let mut rbuf: [u8; 1] = [0];

    let mut arg = door_h::door_arg_t::new(&[111], &[], &mut rbuf);
    double.call(&mut arg).unwrap();
    assert_eq!(rbuf[0], 222);
}
