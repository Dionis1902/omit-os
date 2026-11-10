// SPDX-License-Identifier: GPL-2.0-only
//! The handle table.
//!
//! Windows programs refer to open objects by `HANDLE` values. As on
//! Windows, the values handed out here are small multiples of four, never
//! zero, and never the "invalid handle" value −1; a closed handle's value
//! is not reused immediately, so that using a stale handle is likely to be
//! caught rather than to hit a new object.

use alloc::vec::Vec;

/// A `HANDLE` value.
pub type Handle = usize;

/// `INVALID_HANDLE_VALUE`.
pub const INVALID_HANDLE: Handle = usize::MAX;

const FIRST: Handle = 0x10;
const STEP: Handle = 4;

/// Maps handle values to objects of type `T`.
#[derive(Debug)]
pub struct Table<T> {
    slots: Vec<Option<T>>,
    /// Where the search for a free slot starts.
    next: usize,
    limit: usize,
}

impl<T> Table<T> {
    /// Creates a table that holds at most `limit` objects.
    pub const fn new(limit: usize) -> Self {
        Self {
            slots: Vec::new(),
            next: 0,
            limit,
        }
    }

    fn index(handle: Handle) -> Option<usize> {
        let offset = handle.checked_sub(FIRST)?;
        offset.is_multiple_of(STEP).then_some(offset / STEP)
    }

    /// Stores `object` and returns its handle, or gives the object back if
    /// the table is full.
    pub fn insert(&mut self, object: T) -> Result<Handle, T> {
        // Prefer slots that were never used or were freed longest ago.
        let count = self.slots.len();
        let free = (0..count)
            .map(|offset| (self.next + offset) % count)
            .find(|&index| self.slots[index].is_none());
        let index = match free {
            Some(index) if count >= self.limit => index,
            _ if count < self.limit => {
                self.slots.push(None);
                count
            }
            _ => return Err(object),
        };
        self.slots[index] = Some(object);
        self.next = index + 1;
        Ok(FIRST + index * STEP)
    }

    /// The object `handle` refers to.
    pub fn get(&self, handle: Handle) -> Option<&T> {
        self.slots.get(Self::index(handle)?)?.as_ref()
    }

    /// The object `handle` refers to, mutably.
    pub fn get_mut(&mut self, handle: Handle) -> Option<&mut T> {
        self.slots.get_mut(Self::index(handle)?)?.as_mut()
    }

    /// Removes and returns the object `handle` refers to.
    pub fn remove(&mut self, handle: Handle) -> Option<T> {
        self.slots.get_mut(Self::index(handle)?)?.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hands_out_valid_distinct_handles() {
        let mut table = Table::new(8);
        let a = table.insert("a").unwrap();
        let b = table.insert("b").unwrap();
        assert_ne!(a, b);
        for handle in [a, b] {
            assert!(handle != 0 && handle != INVALID_HANDLE && handle % 4 == 0);
        }
        assert_eq!(table.get(a), Some(&"a"));
        assert_eq!(table.get(b), Some(&"b"));
        *table.get_mut(a).unwrap() = "A";
        assert_eq!(table.remove(a), Some("A"));
        assert_eq!(table.get(a), None);
        assert_eq!(table.remove(a), None, "double close is detected");
    }

    #[test]
    fn rejects_values_that_are_not_handles() {
        let mut table = Table::new(8);
        let a = table.insert(1).unwrap();
        for bogus in [0, 1, a + 1, a + 4, INVALID_HANDLE, INVALID_HANDLE - 1, 4, 8] {
            assert_eq!(table.get(bogus), None, "{bogus:#x}");
            assert_eq!(table.remove(bogus), None);
        }
    }

    #[test]
    fn respects_the_limit_and_delays_reuse() {
        let mut table = Table::new(3);
        let a = table.insert(1).unwrap();
        let b = table.insert(2).unwrap();
        table.remove(a);
        // A free slot exists, but the table may still grow: the stale
        // value of `a` stays invalid.
        let c = table.insert(3).unwrap();
        assert_ne!(c, a);
        let d = table.insert(4).unwrap();
        assert_eq!(d, a, "full table: the freed slot is used");
        assert_eq!(table.insert(5), Err(5));
        table.remove(b);
        assert_eq!(table.insert(6), Ok(b));
    }
}
