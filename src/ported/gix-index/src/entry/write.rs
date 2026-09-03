use crate::{Entry, State, entry};

/// `encode_varint()` (varint.c:19): git's offset encoding, most-significant group
/// first, with every continuation group biased by one so that each length has a
/// single representation.
///
/// ```c
/// int encode_varint(uintmax_t value, unsigned char *buf)
/// {
///         unsigned char varint[16];
///         unsigned pos = sizeof(varint) - 1;
///         varint[pos] = value & 127;
///         while (value >>= 7)
///                 varint[--pos] = 128 | (--value & 127);
///         if (buf)
///                 memcpy(buf, varint + pos, sizeof(varint) - pos);
///         return sizeof(varint) - pos;
/// }
/// ```
fn encode_varint(mut value: u64) -> Vec<u8> {
    let mut varint = [0u8; 16];
    let mut pos = varint.len() - 1;
    varint[pos] = (value & 127) as u8;
    loop {
        value >>= 7;
        if value == 0 {
            break;
        }
        value -= 1;
        pos -= 1;
        varint[pos] = 128 | (value & 127) as u8;
    }
    varint[pos..].to_vec()
}

impl Entry {
    /// Serialize ourselves to `out` with path access via `state`, without padding.
    pub fn write_to(&self, out: impl std::io::Write, state: &State) -> std::io::Result<()> {
        self.write_to_version(out, state, None)
    }

    /// `ce_write_entry()` (read-cache.c:2601) for either name encoding.
    ///
    /// `previous_name` is git's `previous_name` strbuf: `None` writes the path in
    /// full (index v2/v3), `Some(buf)` writes it prefix-compressed against the
    /// previous entry's name as index v4 stores it —
    ///
    /// ```c
    /// for (common = 0;
    ///      (ce->name[common] && common < previous_name->len &&
    ///       ce->name[common] == previous_name->buf[common]);
    ///      common++)
    ///         ; /* still matching */
    /// to_remove = previous_name->len - common;
    /// prefix_size = encode_varint(to_remove, to_remove_vi);
    /// ```
    ///
    /// — i.e. how many bytes to strip from the end of the previous name, then the
    /// bytes that differ, NUL-terminated. The buffer is updated to this entry's
    /// full path on the way out, so the caller hands the same one to every entry.
    pub(crate) fn write_to_version(
        &self,
        mut out: impl std::io::Write,
        state: &State,
        previous_name: Option<&mut Vec<u8>>,
    ) -> std::io::Result<()> {
        let stat = self.stat;
        out.write_all(&stat.ctime.secs.to_be_bytes())?;
        out.write_all(&stat.ctime.nsecs.to_be_bytes())?;
        out.write_all(&stat.mtime.secs.to_be_bytes())?;
        out.write_all(&stat.mtime.nsecs.to_be_bytes())?;
        out.write_all(&stat.dev.to_be_bytes())?;
        out.write_all(&stat.ino.to_be_bytes())?;
        out.write_all(&self.mode.bits().to_be_bytes())?;
        out.write_all(&stat.uid.to_be_bytes())?;
        out.write_all(&stat.gid.to_be_bytes())?;
        out.write_all(&stat.size.to_be_bytes())?;
        out.write_all(self.id.as_bytes())?;
        let path = self.path(state);
        let path_len: u16 = if path.len() >= entry::Flags::PATH_LEN.bits() as usize {
            entry::Flags::PATH_LEN.bits() as u16
        } else {
            path.len()
                .try_into()
                .expect("we just checked that the length is smaller than 0xfff")
        };
        out.write_all(&(self.flags.to_storage().bits() | path_len).to_be_bytes())?;
        if self.flags.contains(entry::Flags::EXTENDED) {
            out.write_all(
                &entry::at_rest::FlagsExtended::from_flags(self.flags)
                    .bits()
                    .to_be_bytes(),
            )?;
        }
        match previous_name {
            None => {
                out.write_all(path)?;
                out.write_all(b"\0")
            }
            Some(previous) => {
                let common = path
                    .iter()
                    .zip(previous.iter())
                    .take_while(|(a, b)| a == b)
                    .count();
                let to_remove = previous.len() - common;
                out.write_all(&encode_varint(to_remove as u64))?;
                out.write_all(&path[common..])?;
                out.write_all(b"\0")?;
                previous.clear();
                previous.extend_from_slice(path);
                Ok(())
            }
        }
    }
}
