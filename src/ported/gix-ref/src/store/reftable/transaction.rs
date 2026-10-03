//! Transactions: `reftable_be_transaction_prepare()`, `_abort()` and
//! `_finish()` with `write_transaction_table()` (refs/reftable-backend.c:956-1697).
//!
//! Each stack touched by a transaction gets its own addition, holding that
//! stack's `tables.list` lock from prepare until commit or abort.

use super::{Backend, Error};

/// What a prepared transaction holds per stack until it is committed or
/// dropped, `struct reftable_transaction_data` (refs/reftable-backend.c:951-954).
pub struct TransactionData {
    _additions: Vec<gix_reftable::Addition>,
}

impl Backend {
    /// `reftable_be_transaction_prepare()` (refs/reftable-backend.c:1314-1416).
    #[expect(dead_code, reason = "file store transactions dispatch here once ported")]
    pub(crate) fn transaction_prepare(&self) -> Result<TransactionData, Error> {
        let _ = TransactionData { _additions: Vec::new() };
        Err(Error::unsupported("transaction_prepare"))
    }

    /// `reftable_be_transaction_finish()` (refs/reftable-backend.c:1666-1697).
    #[expect(dead_code, reason = "file store transactions dispatch here once ported")]
    pub(crate) fn transaction_finish(&self, data: TransactionData) -> Result<(), Error> {
        let _ = data;
        Err(Error::unsupported("transaction_finish"))
    }
}
