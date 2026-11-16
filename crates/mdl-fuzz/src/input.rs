//! The fuzzer's bytes read as a structured input: a header of small
//! fields, each target documenting its own, then the rest, usually a
//! payload. A field read past the end reads as zeros, so every input means
//! something, and a crash file can be read by hand.

pub(crate) struct Input<'a> {
    bytes: &'a [u8],
}

impl<'a> Input<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Input { bytes }
    }

    /// The next `count` bytes, fewer at the end.
    pub(crate) fn take(&mut self, count: usize) -> &'a [u8] {
        let (taken, rest) = self.bytes.split_at(count.min(self.bytes.len()));
        self.bytes = rest;
        taken
    }

    /// The next `N` bytes, zeros past the end.
    fn array<const N: usize>(&mut self) -> [u8; N] {
        let mut out = [0; N];
        let taken = self.take(N);
        out[..taken.len()].copy_from_slice(taken);
        out
    }

    pub(crate) fn byte(&mut self) -> u8 {
        self.array::<1>()[0]
    }

    /// The next byte as one of `count` choices (`count` isn't 0).
    pub(crate) fn choose(&mut self, count: usize) -> usize {
        usize::from(self.byte()) % count
    }

    /// The next two bytes, little-endian.
    pub(crate) fn u16(&mut self) -> u16 {
        u16::from_le_bytes(self.array())
    }

    /// The next four bytes, little-endian.
    pub(crate) fn i32(&mut self) -> i32 {
        i32::from_le_bytes(self.array())
    }

    /// The next eight bytes, little-endian.
    pub(crate) fn u64(&mut self) -> u64 {
        u64::from_le_bytes(self.array())
    }

    /// What's left.
    pub(crate) fn rest(self) -> &'a [u8] {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::Input;

    #[test]
    fn fields_past_the_end_read_as_zeros() {
        let mut input = Input::new(&[7, 1, 2]);
        assert_eq!(input.byte(), 7);
        assert_eq!(input.i32(), 0x0201);
        assert_eq!(input.u64(), 0);
        assert_eq!(input.choose(5), 0);
        assert!(input.rest().is_empty());
    }

    #[test]
    fn the_rest_is_what_the_header_leaves() {
        let mut input = Input::new(b"\x03abcd");
        assert_eq!(input.choose(2), 1);
        assert_eq!(input.take(2), b"ab");
        assert_eq!(input.rest(), b"cd");
    }
}
