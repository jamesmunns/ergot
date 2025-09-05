use core::num::NonZeroU32;

use const_fnv1a_hash::fnv1a_hash_32;
use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "defmt-v1", derive(defmt::Format))]
#[derive(Serialize, Deserialize, Schema, Debug, PartialEq, Clone, Copy)]
#[repr(transparent)]
pub struct NameHash {
    hash: NonZeroU32,
}

impl NameHash {
    pub const fn new(s: &str) -> Self {
        // ensure that an empty string does NOT hash to zero
        let _: () = const {
            assert!(0 != fnv1a_hash_32(&[], None));
        };

        let hash32 = fnv1a_hash_32(s.as_bytes(), None);
        match NonZeroU32::new(hash32) {
            Some(hash) => Self { hash },
            None => {
                // If the hash comes to zero, instead use the len + first three characters
                //
                // We know the len is non-zero because we checked that an empty string above
                // hashes to a non-zero value.
                let len = if s.len() > 255 { 255 } else { s.len() } as u8;
                let mut bytes = [len, 0, 0, 0];

                // Awkward because const
                match s.as_bytes() {
                    [] => unreachable!(),
                    [one] => {
                        bytes[1] = *one;
                        bytes[2] = *one;
                        bytes[3] = *one;
                    }
                    [one, two] => {
                        bytes[1] = *one;
                        bytes[2] = *two;
                        bytes[3] = *one;
                    }
                    [one, two, three] => {
                        bytes[1] = *one;
                        bytes[2] = *two;
                        bytes[3] = *three;
                    }
                    [one, two, three, ..] => {
                        bytes[1] = *one;
                        bytes[2] = *two;
                        bytes[3] = *three;
                    }
                }
                let val = u32::from_le_bytes(bytes);

                // Safety:
                //
                // We know the len is nonzero, which means at least one byte has nonzero
                // contents.
                let hash = unsafe { NonZeroU32::new_unchecked(val) };
                Self { hash }
            }
        }
    }

    pub fn to_u32(&self) -> u32 {
        self.hash.into()
    }

    pub fn from_u32(val: u32) -> Option<Self> {
        let nz = NonZeroU32::new(val)?;
        Some(Self { hash: nz })
    }
}

#[cfg(test)]
mod test {
    use crate::nash::NameHash;

    #[test]
    fn transparent_works() {
        assert_eq!(size_of::<NameHash>(), 4);
        assert_eq!(size_of::<Option<NameHash>>(), 4);
    }
}
