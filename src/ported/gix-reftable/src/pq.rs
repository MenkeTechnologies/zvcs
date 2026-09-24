//! `pq.c`: the binary min-heap a merged iterator orders its sub-iterators by.
//!
//! C heap entries carry a pointer to their sub-iterator's current record;
//! here an entry is the sub-iterator's index and the records are passed in.

use std::cmp::Ordering;

use crate::{Result, record::Record};

/// `struct merged_iter_pqueue`.
#[derive(Default)]
pub(crate) struct MergedIterPqueue {
    heap: Vec<usize>,
}

/// `pq_less()` (`pq.c:16-27`): order by key; for equal keys the entry of the
/// later table — the higher index — comes first, so it shadows older tables.
fn pq_less(a: usize, b: usize, recs: &[Record]) -> Result<bool> {
    Ok(match recs[a].cmp_key(&recs[b])? {
        Ordering::Equal => a > b,
        ord => ord == Ordering::Less,
    })
}

impl MergedIterPqueue {
    /// `merged_iter_pqueue_is_empty()`.
    pub(crate) fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }

    /// `merged_iter_pqueue_top()`: the index of the smallest entry.
    pub(crate) fn top(&self) -> usize {
        self.heap[0]
    }

    /// `merged_iter_pqueue_remove()` (`pq.c:29-68`): pop the smallest entry.
    pub(crate) fn remove(&mut self, recs: &[Record]) -> Result<usize> {
        let e = self.heap.swap_remove(0);
        let len = self.heap.len();
        let mut i = 0;
        while i < len {
            let mut min = i;
            let j = 2 * i + 1;
            let k = 2 * i + 2;
            if j < len && pq_less(self.heap[j], self.heap[i], recs)? {
                min = j;
            }
            if k < len && pq_less(self.heap[k], self.heap[min], recs)? {
                min = k;
            }
            if min == i {
                break;
            }
            self.heap.swap(i, min);
            i = min;
        }
        Ok(e)
    }

    /// `merged_iter_pqueue_add()` (`pq.c:70-89`).
    pub(crate) fn add(&mut self, index: usize, recs: &[Record]) -> Result<()> {
        self.heap.push(index);
        let mut i = self.heap.len() - 1;
        while i > 0 {
            let j = (i - 1) / 2;
            if pq_less(self.heap[j], self.heap[i], recs)? {
                break;
            }
            self.heap.swap(j, i);
            i = j;
        }
        Ok(())
    }

    /// `merged_iter_pqueue_release()`.
    pub(crate) fn clear(&mut self) {
        self.heap.clear();
    }
}
