// Regression test for #135209.
// We ensure that we don't try to access fields on a non-struct pattern type.
fn main() {
    if let Iterator::Item { .. } = 1 { //~ ERROR E0223
        x //~ ERROR E0425
    }
}
