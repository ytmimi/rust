#![warn(clippy::double_ended_iterator_last)]

// Typical case
pub fn last_arg(s: &str) -> Option<&str> {
    s.split(' ').last()
}

fn main() {
    // General case
    struct DeIterator;
    impl Iterator for DeIterator {
        type Item = ();
        fn next(&mut self) -> Option<Self::Item> {
            Some(())
        }
    }
    impl DoubleEndedIterator for DeIterator {
        fn next_back(&mut self) -> Option<Self::Item> {
            Some(())
        }
    }
    let _ = DeIterator.last();
    // Should not apply to other methods of Iterator
    let _ = DeIterator.count();

    // Should not apply to simple iterators
    struct SimpleIterator;
    impl Iterator for SimpleIterator {
        type Item = ();
        fn next(&mut self) -> Option<Self::Item> {
            Some(())
        }
    }
    let _ = SimpleIterator.last();
}
