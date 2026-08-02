// A Request<'_, NoDescriptors> has no method that reaches a
// descriptor. The macro picks this parameter from `refuse_desc`, so a
// door that refuses descriptors cannot accidentally read one.
fn main() {}

fn peek(req: doors::Request<'_, doors::NoDescriptors>) {
    let _ = req.descriptors();
}
