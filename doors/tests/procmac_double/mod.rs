use doors::Client;
use doors::DoorArgument;

#[test]
fn procedural_macro_double_u8() {
    let double = Client::open("/tmp/procmac_double.door").unwrap();

    let mut rbuf: [u8; 1] = [0];

    let arg = DoorArgument::new(&[111], &[], &mut rbuf);
    double.call(arg).unwrap();
    assert_eq!(rbuf[0], 222);
}
