mod locate {
    use bstr::ByteSlice;
    use gix_object::Kind;
    use gix_odb::pack;

    use crate::{SMALL_PACK_INDEX, fixture_path, hex_to_id};

    fn locate<'a>(hex_id: &str, out: &'a mut Vec<u8>) -> gix_object::Data<'a> {
        let bundle = pack::Bundle::at(fixture_path(SMALL_PACK_INDEX), gix_hash::Kind::Sha1).expect("pack and idx");
        bundle
            .find(
                &hex_to_id(hex_id),
                out,
                &mut gix_zlib::Inflate::default(),
                &mut pack::cache::Never,
            )
            .expect("read success")
            .expect("id present")
            .0
    }

    mod locate_and_verify {
        use gix_odb::pack;

        use crate::{PACKS_AND_INDICES, fixture_path};

        #[test]
        fn all() -> Result<(), Box<dyn std::error::Error>> {
            for (index_path, data_path) in PACKS_AND_INDICES {
                // both paths are equivalent
                pack::Bundle::at(fixture_path(index_path), gix_hash::Kind::Sha1)?;
                let bundle = pack::Bundle::at(fixture_path(data_path), gix_hash::Kind::Sha1)?;

                let mut buf = Vec::new();
                for entry in bundle.index.iter() {
                    let (obj, _location) = bundle
                        .find(
                            &entry.oid,
                            &mut buf,
                            &mut gix_zlib::Inflate::default(),
                            &mut pack::cache::Never,
                        )?
                        .expect("id present");
                    obj.verify_checksum(&entry.oid)?;
                }
            }
            Ok(())
        }
    }

    #[test]
    fn blob() -> Result<(), Box<dyn std::error::Error>> {
        let mut out = Vec::new();
        let obj = locate("bd46bb3f5bb4ca5431770c4fde0735fb89d382f3", &mut out);

        assert_eq!(
            obj.data.as_bstr(),
            b"GitPython is a python library used to interact with Git repositories.\n\nHi there\n".as_bstr()
        );
        assert_eq!(obj.kind, Kind::Blob);
        let object = obj.decode()?;
        assert_eq!(object.kind(), Kind::Blob);
        assert_eq!(object.as_blob().expect("blob").data, obj.data);
        Ok(())
    }

    #[test]
    fn tree() -> Result<(), Box<dyn std::error::Error>> {
        let mut out = Vec::new();
        let obj = locate("e90926b07092bccb7bf7da445fae6ffdfacf3eae", &mut out);

        assert_eq!(obj.kind, Kind::Tree);
        assert_eq!(obj.decode()?.kind(), Kind::Tree);
        Ok(())
    }

    #[test]
    fn commit() -> Result<(), Box<dyn std::error::Error>> {
        let mut out = Vec::new();
        let obj = locate("779c5451ba9fe210ffd1f55db202e55f51acecac", &mut out);

        assert_eq!(obj.kind, Kind::Commit);
        assert_eq!(obj.decode()?.kind(), Kind::Commit);
        Ok(())
    }
}

#[cfg(all(not(feature = "wasm"), feature = "streaming-input"))]
mod write_to_directory {
    use std::{fs, path::Path, sync::atomic::AtomicBool};

    use gix_features::progress;
    use gix_odb::pack;
    use gix_testtools::tempfile::TempDir;

    use crate::{SMALL_PACK, SMALL_PACK_INDEX, error_chain_contains_message, fixture_path};

    fn expected_outcome() -> Result<pack::bundle::write::Outcome, Box<dyn std::error::Error>> {
        Ok(pack::bundle::write::Outcome {
            index: pack::index::write::Outcome {
                index_version: pack::index::Version::V2,
                index_hash: gix_hash::ObjectId::from_hex(b"544a7204a55f6e9cacccf8f6e191ea8f83575de3")?,
                data_hash: gix_hash::ObjectId::from_hex(b"0f3ea84cd1bba10c2a03d736a460635082833e59")?,
                num_objects: 42,
            },
            pack_version: pack::data::Version::V2,
            index_path: None,
            data_path: None,
            keep_path: None,
            object_hash: gix_hash::Kind::Sha1,
        })
    }

    #[test]
    fn without_providing_one() -> Result<(), Box<dyn std::error::Error>> {
        let res = write_pack(None::<&Path>, SMALL_PACK)?;
        assert_eq!(res, expected_outcome()?);
        assert_eq!(
            res.index.index_hash,
            pack::index::File::at(fixture_path(SMALL_PACK_INDEX), gix_hash::Kind::Sha1)?.index_checksum()
        );
        assert!(res.to_bundle().is_none());
        Ok(())
    }

    #[test]
    fn given_a_directory() -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let mut res = write_pack(Some(&dir), SMALL_PACK)?;
        let (index_path, data_path, keep_path) = (res.index_path.take(), res.data_path.take(), res.keep_path.take());
        assert_eq!(res, expected_outcome()?);
        let mut sorted_entries = fs::read_dir(&dir)?.filter_map(Result::ok).collect::<Vec<_>>();
        sorted_entries.sort_by_key(fs::DirEntry::file_name);
        assert_eq!(
            sorted_entries.len(),
            3,
            "we want a pack and the corresponding index and the keep file"
        );

        let pack_hash = res.index.data_hash.to_hex();
        assert_eq!(file_name(&sorted_entries[0]), format!("pack-{pack_hash}.idx"));
        assert_eq!(Some(sorted_entries[0].path()), index_path);
        assert_eq!(file_name(&sorted_entries[1]), format!("pack-{pack_hash}.keep"));
        assert_eq!(Some(sorted_entries[1].path()), keep_path);
        assert_eq!(file_name(&sorted_entries[2]), format!("pack-{pack_hash}.pack"));
        assert_eq!(Some(sorted_entries[2].path()), data_path);

        res.index_path = index_path;
        assert!(res.to_bundle().transpose()?.is_some());
        Ok(())
    }

    #[test]
    fn respects_alloc_limit_bytes() -> Result<(), Box<dyn std::error::Error>> {
        let pack_file = fs::File::open(fixture_path(SMALL_PACK))?;
        static SHOULD_INTERRUPT: AtomicBool = AtomicBool::new(false);

        let prevent_allocation = Some(0);
        let err = pack::Bundle::write_to_directory_eagerly(
            Box::new(pack_file),
            None,
            None::<&Path>,
            &mut progress::Discard,
            &SHOULD_INTERRUPT,
            None::<gix_object::find::Never>,
            pack::bundle::write::Options {
                thread_limit: None,
                iteration_mode: pack::data::input::Mode::Verify,
                index_version: pack::index::Version::V2,
                object_hash: gix_hash::Kind::Sha1,
                alloc_limit_bytes: prevent_allocation,
                compression: gix_zlib::Compression::BEST_SPEED,
            },
        )
        .expect_err("a zero allocation limit rejects the first non-empty decoded object");

        assert!(
            error_chain_contains_message(&err, "Entry too large to fit in memory"),
            "bundle writing must forward its allocation limit to index writing"
        );
        Ok(())
    }

    /// A pack whose deltas name their base by object id rather than by offset is what `pack-objects` emits without
    /// `--delta-base-offset`, and `git index-pack` accepts it. Consuming one used to fail outright, which meant a clone
    /// from a server that emits `OBJ_REF_DELTA` could not be indexed. The chain here is deliberately two deep so that
    /// the second delta's base is itself a ref-delta, whose id is only known once the first delta has been applied.
    #[test]
    fn chained_in_pack_ref_deltas() -> Result<(), Box<dyn std::error::Error>> {
        let base = b"the quick brown fox jumps over the lazy dog, and does so repeatedly".to_vec();
        let one = [base.clone(), b"-one".to_vec()].concat();
        let two = [one.clone(), b"-two".to_vec()].concat();
        let id = |data: &[u8]| gix_hash::ObjectId::from(gix_object::compute_hash(gix_hash::Kind::Sha1, gix_object::Kind::Blob, data).expect("hashing blobs never fails"));

        let mut pack = pack_header(3);
        append_entry(&mut pack, pack::data::entry::Header::Blob, base.len() as u64, &base);
        append_entry(
            &mut pack,
            pack::data::entry::Header::RefDelta { base_id: id(&base) },
            0,
            &append_delta(&base, b"-one"),
        );
        append_entry(
            &mut pack,
            pack::data::entry::Header::RefDelta { base_id: id(&one) },
            0,
            &append_delta(&one, b"-two"),
        );
        seal_pack(&mut pack);

        let dir = TempDir::new()?;
        static SHOULD_INTERRUPT: AtomicBool = AtomicBool::new(false);
        let outcome = pack::Bundle::write_to_directory(
            &mut pack.as_slice(),
            Some(dir.path()),
            &mut progress::Discard,
            &SHOULD_INTERRUPT,
            // An empty object database, so nothing can be mistaken for a thin-pack base.
            Some(gix_object::find::Never),
            pack::bundle::write::Options {
                thread_limit: Some(1),
                iteration_mode: pack::data::input::Mode::Verify,
                index_version: pack::index::Version::V2,
                object_hash: gix_hash::Kind::Sha1,
                alloc_limit_bytes: None,
                compression: gix_zlib::Compression::BEST_SPEED,
            },
        )?;
        assert_eq!(outcome.index.num_objects, 3, "no entry was dropped or duplicated");

        let bundle = pack::Bundle::at(outcome.index_path.expect("written to a directory"), gix_hash::Kind::Sha1)?;
        for expected in [&base, &one, &two] {
            let mut buf = Vec::new();
            let (obj, _) = bundle
                .find(
                    &id(expected),
                    &mut buf,
                    &mut gix_zlib::Inflate::default(),
                    &mut pack::cache::Never,
                )?
                .expect("every object of the pack is indexed under its own id");
            assert_eq!(obj.kind, gix_object::Kind::Blob, "deltas inherit the base object's type");
            assert_eq!(obj.data, expected.as_slice(), "the delta chain was applied correctly");
        }
        Ok(())
    }

    fn pack_header(num_objects: u32) -> Vec<u8> {
        let mut out = b"PACK".to_vec();
        out.extend(2u32.to_be_bytes());
        out.extend(num_objects.to_be_bytes());
        out
    }

    fn seal_pack(pack: &mut Vec<u8>) {
        let mut hasher = gix_hash::hasher(gix_hash::Kind::Sha1);
        hasher.update(pack);
        pack.extend(hasher.try_finalize().expect("hashing in memory never fails").as_slice());
    }

    fn append_entry(pack: &mut Vec<u8>, header: pack::data::entry::Header, decompressed_size: u64, payload: &[u8]) {
        let decompressed_size = if decompressed_size == 0 {
            payload.len() as u64
        } else {
            decompressed_size
        };
        header
            .write_to(decompressed_size, pack)
            .expect("writing an entry header to memory succeeds");
        let mut deflate = gix_zlib::stream::deflate::Write::new(Vec::new(), gix_zlib::Compression::BEST_SPEED);
        std::io::Write::write_all(&mut deflate, payload).expect("deflating to memory succeeds");
        std::io::Write::flush(&mut deflate).expect("flushing the deflater succeeds");
        pack.extend(deflate.into_inner());
    }

    /// A delta that copies all of `base` and then inserts `suffix`, the smallest shape that still reads from its base.
    fn append_delta(base: &[u8], suffix: &[u8]) -> Vec<u8> {
        assert!(base.len() < 256 && suffix.len() < 128, "keeps the encoding to one byte per field");
        let mut out = vec![base.len() as u8, (base.len() + suffix.len()) as u8];
        // copy from base: offset and size are one byte each, both present.
        out.extend([0b1001_0001, 0, base.len() as u8]);
        // insert the literal suffix.
        out.push(suffix.len() as u8);
        out.extend(suffix);
        out
    }

    fn file_name(entry: &fs::DirEntry) -> String {
        entry.path().file_name().unwrap().to_str().unwrap().to_owned()
    }

    fn write_pack(
        directory: Option<impl AsRef<Path>>,
        pack_file: &str,
    ) -> Result<pack::bundle::write::Outcome, Box<dyn std::error::Error>> {
        let pack_file = fs::File::open(fixture_path(pack_file))?;
        static SHOULD_INTERRUPT: AtomicBool = AtomicBool::new(false);
        pack::Bundle::write_to_directory_eagerly(
            Box::new(pack_file),
            None,
            directory,
            &mut progress::Discard,
            &SHOULD_INTERRUPT,
            None::<gix_object::find::Never>,
            pack::bundle::write::Options {
                thread_limit: None,
                iteration_mode: pack::data::input::Mode::Verify,
                index_version: pack::index::Version::V2,
                object_hash: gix_hash::Kind::Sha1,
                alloc_limit_bytes: None,
                compression: gix_zlib::Compression::BEST_SPEED,
            },
        )
        .map_err(Into::into)
    }
}
