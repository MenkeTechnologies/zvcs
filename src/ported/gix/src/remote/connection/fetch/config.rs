use super::Error;
use crate::{
    Repository,
    config::{cache::util::ApplyLeniency, tree::Pack},
};

pub fn index_threads(repo: &Repository) -> Result<Option<usize>, Error> {
    Ok(Pack::THREADS
        .try_into_usize(
            repo.config
                .resolved
                .integer_filter(Pack::THREADS, &mut repo.filter_config_section()),
        )
        .with_leniency(repo.options.lenient_config)?)
}

pub fn pack_index_version(repo: &Repository) -> Result<gix_pack::index::Version, Error> {
    Ok(Pack::INDEX_VERSION
        .try_into_index_version(repo.config.resolved.integer(Pack::INDEX_VERSION))
        .with_leniency(repo.options.lenient_config)?
        .unwrap_or(gix_pack::index::Version::V2))
}

/// `unpack_limit` after `fetch_pack_setup()` (fetch-pack.c:2015-2026): `fetch.unpackLimit`, else
/// `transfer.unpackLimit`, each only when not negative, else 100. A pack with fewer objects goes to
/// `unpack-objects`, unless `0` turns that off.
pub fn unpack_limit(repo: &Repository) -> u64 {
    let snapshot = repo.config_snapshot();
    let non_negative = |key: &str| snapshot.integer(key).and_then(|v| u64::try_from(v).ok());
    non_negative("fetch.unpackLimit")
        .or_else(|| non_negative("transfer.unpackLimit"))
        .unwrap_or(100)
}
