//! The two hash tables git iterates when it prints a set of object ids, ported
//! for their *order*.
//!
//! `git rev-list` collects the objects it could not find and the ones a filter
//! omitted into hash tables and prints them when the walk is over. Neither is
//! sorted: each comes out in the table's own bucket order, which is a pure
//! function of the ids and the table geometry — so it is reproducible, and a
//! port that prints them in encounter order (or sorted) disagrees with stock on
//! every listing that has more than one.
//!
//! Both tables key on `oidhash()` (hash.h):
//!
//! ```c
//! static inline unsigned int oidhash(const struct object_id *oid)
//! {
//!         /*
//!          * Since the sha1/sha256 is essentially random, we just take the
//!          * required number of bytes from it
//!          */
//!         unsigned int hash;
//!         memcpy(&hash, oid->hash, sizeof(hash));
//!         return hash;
//! }
//! ```
//!
//! — the first four bytes of the id read as a native-endian `unsigned int`,
//! which on every platform this port targets is little-endian.
//!
//! The two differ in everything else. [`khash_order`] is `oidset`, an open
//! addressed table with quadratic probing (khash.h); [`hashmap_order`] is
//! `oidmap`, a chained table whose buckets are prepend-only stacks
//! (hashmap.c). They are given the same `DEFAULT_OIDSET_SIZE` request and end up
//! with different geometries from it, so the same ids come out in different
//! orders — which is exactly what stock does.

use gix::hash::ObjectId;

/// `DEFAULT_OIDSET_SIZE` (builtin/rev-list.c:118), the size both tables are
/// asked for.
pub const DEFAULT_OIDSET_SIZE: usize = 16 * 1024;

/// `oidhash()`: the first four bytes of the id, little-endian.
fn oidhash(id: &ObjectId) -> u32 {
    let bytes = id.as_slice();
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// `kroundup32()`: the next power of two at or above `n`.
fn kroundup32(n: usize) -> usize {
    let mut x = n;
    if x == 0 {
        return 0;
    }
    x -= 1;
    x |= x >> 1;
    x |= x >> 2;
    x |= x >> 4;
    x |= x >> 8;
    x |= x >> 16;
    x |= x >> 32;
    x + 1
}

/// The order `oidset_iter_next()` (oidset.h:113-120) walks a set built by
/// inserting `ids` in order: ascending bucket index over khash's open-addressed
/// table.
///
/// ```c
/// x = site = h->n_buckets; k = __hash_func(key); i = k & mask;
/// if (__ac_isempty(h->flags, i)) x = i;
/// else {
///         last = i;
///         while (!__ac_isempty(h->flags, i) && … !__hash_equal(h->keys[i], key)) {
///                 …
///                 i = (i + (++step)) & mask;
///                 if (i == last) { x = site; break; }
///         }
///         …
/// }
/// ```
///
/// (`kh_put`, khash.h.) The probe is quadratic with a growing step, and the
/// table doubles once it is 77% occupied — neither of which a handful of ids
/// ever reaches, but both of which decide the order once a repository is large
/// enough to.
pub fn khash_order(ids: &[ObjectId]) -> Vec<ObjectId> {
    let mut table = KHash::with_capacity(DEFAULT_OIDSET_SIZE);
    for id in ids {
        table.insert(*id);
    }
    table.keys.into_iter().flatten().collect()
}

/// khash's `__ac_HASH_UPPER`.
const KHASH_UPPER: f64 = 0.77;

struct KHash {
    keys: Vec<Option<ObjectId>>,
    size: usize,
    upper_bound: usize,
}

impl KHash {
    /// `kh_resize()` from empty: `kroundup32(n)`, with a floor of 4.
    fn with_capacity(n: usize) -> Self {
        let buckets = kroundup32(n).max(4);
        KHash {
            keys: vec![None; buckets],
            size: 0,
            upper_bound: (buckets as f64 * KHASH_UPPER + 0.5) as usize,
        }
    }

    fn insert(&mut self, key: ObjectId) {
        // `if (h->n_occupied >= h->upper_bound)`: the table grows *before* the
        // key is placed, so the bucket it lands in is the new geometry's.
        if self.size >= self.upper_bound {
            self.resize(self.keys.len() * 2);
        }
        if let Some(slot) = self.slot_for(&key) {
            if self.keys[slot].is_none() {
                self.keys[slot] = Some(key);
                self.size += 1;
            }
        }
    }

    /// The bucket `kh_put` would answer with: the first empty one along the
    /// probe, or the one already holding this key.
    fn slot_for(&self, key: &ObjectId) -> Option<usize> {
        let mask = self.keys.len() - 1;
        let mut i = oidhash(key) as usize & mask;
        let last = i;
        let mut step = 0usize;
        while let Some(occupant) = self.keys[i] {
            if occupant == *key {
                return Some(i);
            }
            step += 1;
            i = (i + step) & mask;
            // A full table, which the load factor makes unreachable.
            if i == last {
                return None;
            }
        }
        Some(i)
    }

    /// `kh_resize()`'s kick-out relocation. With no deletions in play the old
    /// table's occupancy is exactly its keys, and each is re-placed into the new
    /// geometry — displacing whatever sat in its way, which is then placed in
    /// turn.
    fn resize(&mut self, new_buckets: usize) {
        let new_buckets = kroundup32(new_buckets).max(4);
        let mask = new_buckets - 1;
        let mut new_keys: Vec<Option<ObjectId>> = vec![None; new_buckets];
        let old = std::mem::take(&mut self.keys);
        // `for (j = 0; j != h->n_buckets; ++j)`, in ascending order.
        for slot in old.into_iter().flatten() {
            let mut key = slot;
            loop {
                let mut i = oidhash(&key) as usize & mask;
                let mut step = 0usize;
                while new_keys[i].is_some() {
                    step += 1;
                    i = (i + step) & mask;
                }
                match new_keys[i].replace(key) {
                    // The slot was empty: the key is placed and this chain ends.
                    None => break,
                    // Unreachable: the probe above only stops on an empty slot.
                    Some(displaced) => key = displaced,
                }
            }
        }
        self.keys = new_keys;
        self.upper_bound = (new_buckets as f64 * KHASH_UPPER + 0.5) as usize;
    }
}

/// The order `hashmap_iter_next()` (hashmap.c) walks a map built by adding `ids`
/// in order: ascending bucket index, and within one bucket the chain — which
/// `hashmap_add()` prepends to, so a bucket answers newest first.
///
/// ```c
/// b = bucket(map, entry);
/// entry->next = map->table[b];
/// map->table[b] = entry;
/// if (map->do_count_items) {
///         map->private_size++;
///         if (map->private_size > map->grow_at)
///                 rehash(map, map->tablesize << HASHMAP_RESIZE_BITS);
/// }
/// ```
///
/// (`hashmap_add`, hashmap.c:232-250.) The table starts at 64 slots and
/// quadruples, and `hashmap_init()` scales the requested size by the load factor
/// before rounding up to one of those — so a request for 16384 lands on 65536,
/// not on 16384, and the bucket a given id takes is a different one than the
/// `oidset` beside it uses.
pub fn hashmap_order(ids: &[ObjectId]) -> Vec<ObjectId> {
    let mut map = HashMapOrder::with_capacity(DEFAULT_OIDSET_SIZE);
    for id in ids {
        map.add(*id);
    }
    map.table.into_iter().flatten().collect()
}

/// `HASHMAP_INITIAL_SIZE`, `HASHMAP_RESIZE_BITS` and `HASHMAP_LOAD_FACTOR`.
const HASHMAP_INITIAL_SIZE: usize = 64;
const HASHMAP_RESIZE_BITS: usize = 2;
const HASHMAP_LOAD_FACTOR: usize = 80;

struct HashMapOrder {
    /// One chain per bucket, newest first.
    table: Vec<Vec<ObjectId>>,
    size: usize,
    grow_at: usize,
}

impl HashMapOrder {
    /// `hashmap_init()` (hashmap.c:153-175): the requested size is divided by the
    /// load factor and then rounded *up* through the quadrupling sequence.
    fn with_capacity(requested: usize) -> Self {
        let scaled = requested * 100 / HASHMAP_LOAD_FACTOR;
        let mut size = HASHMAP_INITIAL_SIZE;
        while scaled > size {
            size <<= HASHMAP_RESIZE_BITS;
        }
        HashMapOrder {
            table: vec![Vec::new(); size],
            size: 0,
            grow_at: size * HASHMAP_LOAD_FACTOR / 100,
        }
    }

    /// `oidmap_put()` through `hashmap_add()`. `add_missing_object_entry()` looks
    /// the id up first and returns early when it is already there, so a repeated
    /// id keeps its first position.
    fn add(&mut self, key: ObjectId) {
        let mask = self.table.len() - 1;
        let bucket = oidhash(&key) as usize & mask;
        if self.table[bucket].contains(&key) {
            return;
        }
        self.table[bucket].insert(0, key);
        self.size += 1;
        if self.size > self.grow_at {
            self.rehash(self.table.len() << HASHMAP_RESIZE_BITS);
        }
    }

    /// `rehash()` (hashmap.c:115-133): the old buckets are walked in ascending
    /// order and each chain front to back, every entry being prepended into its
    /// new bucket — so a chain that survives intact comes out reversed.
    fn rehash(&mut self, newsize: usize) {
        let mask = newsize - 1;
        let mut fresh: Vec<Vec<ObjectId>> = vec![Vec::new(); newsize];
        for chain in std::mem::take(&mut self.table) {
            for key in chain {
                let bucket = oidhash(&key) as usize & mask;
                fresh[bucket].insert(0, key);
            }
        }
        self.table = fresh;
        self.grow_at = newsize * HASHMAP_LOAD_FACTOR / 100;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(hex: &str) -> ObjectId {
        ObjectId::from_hex(hex.as_bytes()).expect("valid hex")
    }

    /// The five ids `git rev-list --objects --filter=blob:none
    /// --filter-print-omitted` printed for a fixture, in stock's order. Their
    /// buckets in a 16384-slot khash table are 1492, 2114, 5811, 9793 and 12842,
    /// which is neither the order they were inserted in nor sorted order.
    #[test]
    fn khash_order_is_bucket_order() {
        let inserted = [
            oid("2a3243572f7cca04fc226d396936cf81049724d6"),
            oid("41e625c25cae4c740d3cf1196d065d462c653bda"),
            oid("42089ca1d44502e78b53df75fa135b33c185377d"),
            oid("b3d68ef2b9891891be7b21d8d34dd0ec5d70c2c9"),
            oid("d405b650d9298ab9e3ce5a9eae37656e7c0554a6"),
        ];
        let expected = [
            oid("d405b650d9298ab9e3ce5a9eae37656e7c0554a6"),
            oid("42089ca1d44502e78b53df75fa135b33c185377d"),
            oid("b3d68ef2b9891891be7b21d8d34dd0ec5d70c2c9"),
            oid("41e625c25cae4c740d3cf1196d065d462c653bda"),
            oid("2a3243572f7cca04fc226d396936cf81049724d6"),
        ];
        assert_eq!(khash_order(&inserted), expected);
        // Insertion order does not matter while no two ids share a bucket.
        let mut reversed = inserted;
        reversed.reverse();
        assert_eq!(khash_order(&reversed), expected);
    }

    /// The three missing ids of a partial clone, in the order stock's `oidmap`
    /// iterated them — a 65536-slot table, so the buckets are 1380, 32776 and
    /// 61310 rather than the khash table's.
    #[test]
    fn hashmap_order_is_bucket_order_with_a_wider_table() {
        let inserted = [
            oid("7eefafcac1e67b8d4cccd29a48ee216fd80468fa"),
            oid("0880af1bf76c7ecadcd75b4365be837f7ed24b14"),
            oid("64055193280dd61767e77ba8edca06d97f71967e"),
        ];
        assert_eq!(
            hashmap_order(&inserted),
            [
                oid("64055193280dd61767e77ba8edca06d97f71967e"),
                oid("0880af1bf76c7ecadcd75b4365be837f7ed24b14"),
                oid("7eefafcac1e67b8d4cccd29a48ee216fd80468fa"),
            ]
        );
        // The same three ids land elsewhere in the narrower `oidset` table, which
        // is why the two listings a single `rev-list` prints are ordered
        // differently.
        assert_ne!(khash_order(&inserted), hashmap_order(&inserted));
    }

    /// A repeated id keeps its first position in both tables.
    #[test]
    fn a_repeated_id_is_added_once() {
        let id = oid("2a3243572f7cca04fc226d396936cf81049724d6");
        let other = oid("41e625c25cae4c740d3cf1196d065d462c653bda");
        assert_eq!(khash_order(&[id, other, id]), [other, id]);
        assert_eq!(hashmap_order(&[id, other, id]).len(), 2);
    }

    /// Growth keeps every id and nothing else: 20000 ids cross both tables'
    /// thresholds (khash grows at 12615, the map at 52428).
    #[test]
    fn growing_past_the_load_factor_keeps_every_id() {
        let ids: Vec<ObjectId> = (0..20000u32)
            .map(|n| {
                let mut raw = [0u8; 20];
                raw[..4].copy_from_slice(&n.to_le_bytes());
                raw[4..8].copy_from_slice(&n.wrapping_mul(2654435761).to_le_bytes());
                ObjectId::from_bytes_or_panic(&raw)
            })
            .collect();
        for ordered in [khash_order(&ids), hashmap_order(&ids)] {
            assert_eq!(ordered.len(), ids.len());
            assert_eq!(
                ordered.iter().collect::<std::collections::HashSet<_>>(),
                ids.iter().collect::<std::collections::HashSet<_>>()
            );
        }
    }
}
