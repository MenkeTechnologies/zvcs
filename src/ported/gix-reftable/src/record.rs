//! `record.c`: the four record kinds and their key/value encodings.
//!
//! C dispatches through a `reftable_record_vtable` per type; here [`Record`] is
//! an enum whose methods match on the variant, one arm per vtable entry.

use std::cmp::Ordering;

use bstr::BString;

use crate::{
    BLOCK_TYPE_INDEX, BLOCK_TYPE_LOG, BLOCK_TYPE_OBJ, BLOCK_TYPE_REF, Error, Result,
    basics::{HASH_SIZE_MAX, common_prefix_size, get_be16, get_be64},
};

/// A fixed-size object ID buffer; only the first `hash_size` bytes are meaningful.
pub type Hash = [u8; HASH_SIZE_MAX];

/// `get_var_int()` (`record.c:22-54`): decode a varint from the front of `input`,
/// returning the value and the number of bytes read, or `None` on truncation or
/// overflow.
pub(crate) fn get_var_int(input: &[u8]) -> Option<(u64, usize)> {
    let mut pos = 0;
    let mut c = *input.get(pos)?;
    pos += 1;
    let mut val = u64::from(c & 0x7f);

    while c & 0x80 != 0 {
        // Whenever the 0x80 bit is set the remainder cannot be 0, so encoding
        // subtracts 1 and decoding adds it back, saving a byte in edge cases.
        val = val.wrapping_add(1);
        if val == 0 || (val & (!0u64 << (64 - 7))) != 0 {
            return None; // overflow
        }
        c = *input.get(pos)?;
        pos += 1;
        val = (val << 7) + u64::from(c & 0x7f);
    }
    Some((val, pos))
}

/// `put_var_int()` (`record.c:56-67`): encode `value` at the front of `dest`,
/// returning the number of bytes written.
pub(crate) fn put_var_int(dest: &mut [u8], mut value: u64) -> Result<usize> {
    let mut varint = [0u8; 10];
    let mut pos = varint.len() - 1;
    varint[pos] = (value & 0x7f) as u8;
    loop {
        value >>= 7;
        if value == 0 {
            break;
        }
        value -= 1;
        pos -= 1;
        varint[pos] = 0x80 | (value & 0x7f) as u8;
    }
    let n = varint.len() - pos;
    if dest.len() < n {
        return Err(Error::EntryTooBig);
    }
    dest[..n].copy_from_slice(&varint[pos..]);
    Ok(n)
}

/// `reftable_is_block_type()` (`record.c:69-79`).
pub(crate) fn is_block_type(typ: u8) -> bool {
    matches!(typ, BLOCK_TYPE_REF | BLOCK_TYPE_LOG | BLOCK_TYPE_OBJ | BLOCK_TYPE_INDEX)
}

/// `decode_string()` (`record.c:103-124`): a varint length followed by that many
/// bytes, copied into `dest`.
fn decode_string(dest: &mut Vec<u8>, input: &[u8]) -> Option<usize> {
    let (tsize, n) = get_var_int(input)?;
    if n == 0 {
        return None;
    }
    let rest = &input[n..];
    let tsize = usize::try_from(tsize).ok()?;
    if rest.len() < tsize {
        return None;
    }
    dest.clear();
    dest.extend_from_slice(&rest[..tsize]);
    Some(n + tsize)
}

/// `encode_string()` (`record.c:126-140`).
fn encode_string(s: &[u8], dest: &mut [u8]) -> Result<usize> {
    let n = put_var_int(dest, s.len() as u64)?;
    let rest = &mut dest[n..];
    if rest.len() < s.len() {
        return Err(Error::EntryTooBig);
    }
    rest[..s.len()].copy_from_slice(s);
    Ok(n + s.len())
}

/// `reftable_encode_key()` (`record.c:142-167`): prefix-compress `key` against
/// `prev_key`. Returns the bytes written and whether the entry is a restart point
/// (it shares no prefix).
pub(crate) fn encode_key(dest: &mut [u8], prev_key: &[u8], key: &[u8], extra: u8) -> Result<(usize, bool)> {
    let prefix_len = common_prefix_size(prev_key, key);
    let suffix_len = key.len() - prefix_len;
    let mut pos = put_var_int(dest, prefix_len as u64)?;
    let is_restart = prefix_len == 0;

    pos += put_var_int(&mut dest[pos..], ((suffix_len as u64) << 3) | u64::from(extra))?;

    let rest = &mut dest[pos..];
    if rest.len() < suffix_len {
        return Err(Error::EntryTooBig);
    }
    rest[..suffix_len].copy_from_slice(&key[prefix_len..]);
    Ok((pos + suffix_len, is_restart))
}

/// `reftable_decode_keylen()` (`record.c:169-191`): returns
/// `(prefix_len, suffix_len, extra, bytes_read)`.
pub(crate) fn decode_keylen(input: &[u8]) -> Option<(u64, u64, u8, usize)> {
    let (prefix_len, n1) = get_var_int(input)?;
    let (suffix_len, n2) = get_var_int(&input[n1..])?;
    if n2 == 0 {
        return None;
    }
    let extra = (suffix_len & 0x7) as u8;
    Some((prefix_len, suffix_len >> 3, extra, n1 + n2))
}

/// `reftable_decode_key()` (`record.c:193-221`): decode a key into `last_key`,
/// which holds the preceding record's key. Returns `(extra, bytes_read)`.
pub(crate) fn decode_key(last_key: &mut Vec<u8>, input: &[u8]) -> Option<(u8, usize)> {
    let (prefix_len, suffix_len, extra, n) = decode_keylen(input)?;
    let rest = &input[n..];
    let prefix_len = usize::try_from(prefix_len).ok()?;
    let suffix_len = usize::try_from(suffix_len).ok()?;
    if rest.len() < suffix_len || prefix_len > last_key.len() {
        return None;
    }
    last_key.truncate(prefix_len);
    last_key.extend_from_slice(&rest[..suffix_len]);
    Some((extra, n + suffix_len))
}

/// The value of a [`RefRecord`], `value_type` plus the `value` union of
/// `struct reftable_ref_record` (`reftable-record.h:23-50`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RefValue {
    /// `REFTABLE_REF_DELETION`: a tombstone hiding the ref in older tables.
    #[default]
    Deletion,
    /// `REFTABLE_REF_VAL1`: a plain ref.
    Val1(Hash),
    /// `REFTABLE_REF_VAL2`: a ref to a tag, with the tag's peeled target.
    Val2 {
        /// The object the ref points to.
        value: Hash,
        /// The fully peeled object.
        target_value: Hash,
    },
    /// `REFTABLE_REF_SYMREF`: a symbolic ref and its referent.
    Symref(BString),
}

impl RefValue {
    fn val_type(&self) -> u8 {
        match self {
            RefValue::Deletion => 0,
            RefValue::Val1(_) => 1,
            RefValue::Val2 { .. } => 2,
            RefValue::Symref(_) => 3,
        }
    }
}

/// `struct reftable_ref_record`: a ref database entry.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RefRecord {
    /// The name of the ref.
    pub refname: BString,
    /// The logical timestamp at which this value was written.
    pub update_index: u64,
    /// What the ref holds.
    pub value: RefValue,
}

impl RefRecord {
    /// `reftable_ref_record_val1()`: the first hash of a `VAL1` or `VAL2` record.
    pub fn val1(&self) -> Option<&Hash> {
        match &self.value {
            RefValue::Val1(h) | RefValue::Val2 { value: h, .. } => Some(h),
            _ => None,
        }
    }

    /// `reftable_ref_record_val2()`: the peeled hash of a `VAL2` record.
    pub fn val2(&self) -> Option<&Hash> {
        match &self.value {
            RefValue::Val2 { target_value, .. } => Some(target_value),
            _ => None,
        }
    }

    /// `reftable_ref_record_is_deletion()`.
    pub fn is_deletion(&self) -> bool {
        matches!(self.value, RefValue::Deletion)
    }

    /// `reftable_ref_record_equal()` (`record.c:1219-1244`), comparing only the
    /// first `hash_size` bytes of each hash.
    pub fn equal(&self, other: &Self, hash_size: usize) -> bool {
        if self.refname != other.refname || self.update_index != other.update_index {
            return false;
        }
        match (&self.value, &other.value) {
            (RefValue::Deletion, RefValue::Deletion) => true,
            (RefValue::Val1(a), RefValue::Val1(b)) => a[..hash_size] == b[..hash_size],
            (
                RefValue::Val2 { value: a, target_value: ta },
                RefValue::Val2 { value: b, target_value: tb },
            ) => a[..hash_size] == b[..hash_size] && ta[..hash_size] == tb[..hash_size],
            (RefValue::Symref(a), RefValue::Symref(b)) => a == b,
            _ => false,
        }
    }

    /// `reftable_ref_record_encode()` (`record.c:319-358`).
    fn encode(&self, s: &mut [u8], hash_size: usize) -> Result<usize> {
        let mut pos = put_var_int(s, self.update_index)?;
        match &self.value {
            RefValue::Symref(target) => pos += encode_string(target, &mut s[pos..])?,
            RefValue::Val2 { value, target_value } => {
                if s.len() - pos < 2 * hash_size {
                    return Err(Error::EntryTooBig);
                }
                s[pos..pos + hash_size].copy_from_slice(&value[..hash_size]);
                pos += hash_size;
                s[pos..pos + hash_size].copy_from_slice(&target_value[..hash_size]);
                pos += hash_size;
            }
            RefValue::Val1(value) => {
                if s.len() - pos < hash_size {
                    return Err(Error::EntryTooBig);
                }
                s[pos..pos + hash_size].copy_from_slice(&value[..hash_size]);
                pos += hash_size;
            }
            RefValue::Deletion => {}
        }
        Ok(pos)
    }

    /// `reftable_ref_record_decode()` (`record.c:360-437`).
    fn decode(&mut self, key: &[u8], val_type: u8, input: &[u8], hash_size: usize, scratch: &mut Vec<u8>) -> Result<usize> {
        let (update_index, mut pos) = get_var_int(input).ok_or(Error::Format)?;

        self.refname.clear();
        self.refname.extend_from_slice(key);
        self.update_index = update_index;
        let input = &input[pos..];
        self.value = match val_type {
            1 => {
                if input.len() < hash_size {
                    return Err(Error::Format);
                }
                let mut h = [0; HASH_SIZE_MAX];
                h[..hash_size].copy_from_slice(&input[..hash_size]);
                pos += hash_size;
                RefValue::Val1(h)
            }
            2 => {
                if input.len() < 2 * hash_size {
                    return Err(Error::Format);
                }
                let mut value = [0; HASH_SIZE_MAX];
                let mut target_value = [0; HASH_SIZE_MAX];
                value[..hash_size].copy_from_slice(&input[..hash_size]);
                target_value[..hash_size].copy_from_slice(&input[hash_size..2 * hash_size]);
                pos += 2 * hash_size;
                RefValue::Val2 { value, target_value }
            }
            3 => {
                let n = decode_string(scratch, input).ok_or(Error::Format)?;
                pos += n;
                RefValue::Symref(std::mem::take(scratch).into())
            }
            0 => RefValue::Deletion,
            // C aborts on a value type it does not know; a corrupt table is not
            // a reason to take the process down here.
            _ => return Err(Error::Format),
        };
        Ok(pos)
    }
}

/// The payload of a non-deletion [`LogRecord`] (`value.update` of
/// `struct reftable_log_record`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LogUpdate {
    /// The object the ref pointed to after the update.
    pub new_hash: Hash,
    /// The object the ref pointed to before the update.
    pub old_hash: Hash,
    /// The committer name.
    pub name: BString,
    /// The committer email, without angle brackets.
    pub email: BString,
    /// Seconds since the epoch.
    pub time: u64,
    /// The timezone offset as `HHMM`, signed.
    pub tz_offset: i16,
    /// The reflog message.
    pub message: BString,
}

/// `value_type` plus `value` of `struct reftable_log_record`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum LogValue {
    /// `REFTABLE_LOG_DELETION`: a tombstone hiding the entry in older tables.
    #[default]
    Deletion,
    /// `REFTABLE_LOG_UPDATE`.
    Update(LogUpdate),
}

/// `struct reftable_log_record`: one reflog entry.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LogRecord {
    /// The ref this entry belongs to.
    pub refname: BString,
    /// The logical timestamp of the transaction that wrote the entry.
    pub update_index: u64,
    /// The entry itself.
    pub value: LogValue,
}

impl LogRecord {
    /// `reftable_log_record_is_deletion()`.
    pub fn is_deletion(&self) -> bool {
        matches!(self.value, LogValue::Deletion)
    }

    /// The update payload, if this is not a deletion.
    pub fn update(&self) -> Option<&LogUpdate> {
        match &self.value {
            LogValue::Update(u) => Some(u),
            LogValue::Deletion => None,
        }
    }

    /// `reftable_log_record_equal()` (`record.c:997-1023`).
    pub fn equal(&self, other: &Self, hash_size: usize) -> bool {
        if self.refname != other.refname || self.update_index != other.update_index {
            return false;
        }
        match (&self.value, &other.value) {
            (LogValue::Deletion, LogValue::Deletion) => true,
            (LogValue::Update(a), LogValue::Update(b)) => {
                a.name == b.name
                    && a.time == b.time
                    && a.tz_offset == b.tz_offset
                    && a.email == b.email
                    && a.message == b.message
                    && a.old_hash[..hash_size] == b.old_hash[..hash_size]
                    && a.new_hash[..hash_size] == b.new_hash[..hash_size]
            }
            _ => false,
        }
    }

    /// `reftable_log_record_compare_key()` (`record.c:1257-1268`): by name,
    /// then by *decreasing* update index.
    pub fn compare_key(&self, other: &Self) -> Ordering {
        self.refname
            .cmp(&other.refname)
            .then_with(|| other.update_index.cmp(&self.update_index))
    }

    /// `reftable_log_record_key()` (`record.c:673-694`): the name, a NUL, and the
    /// inverted update index in big endian, so newer entries sort first.
    fn key(&self, dest: &mut Vec<u8>) {
        dest.clear();
        dest.extend_from_slice(&self.refname);
        dest.push(0);
        dest.extend_from_slice(&(!self.update_index).to_be_bytes());
    }

    /// `reftable_log_record_encode()` (`record.c:777-822`).
    fn encode(&self, s: &mut [u8], hash_size: usize) -> Result<usize> {
        let LogValue::Update(u) = &self.value else {
            return Ok(0);
        };
        if s.len() < 2 * hash_size {
            return Err(Error::EntryTooBig);
        }
        s[..hash_size].copy_from_slice(&u.old_hash[..hash_size]);
        s[hash_size..2 * hash_size].copy_from_slice(&u.new_hash[..hash_size]);
        let mut pos = 2 * hash_size;

        pos += encode_string(&u.name, &mut s[pos..])?;
        pos += encode_string(&u.email, &mut s[pos..])?;
        pos += put_var_int(&mut s[pos..], u.time)?;

        if s.len() - pos < 2 {
            return Err(Error::EntryTooBig);
        }
        s[pos..pos + 2].copy_from_slice(&(u.tz_offset as u16).to_be_bytes());
        pos += 2;

        pos += encode_string(&u.message, &mut s[pos..])?;
        Ok(pos)
    }

    /// `reftable_log_record_decode()` (`record.c:824-959`).
    fn decode(&mut self, key: &[u8], val_type: u8, input: &[u8], hash_size: usize, scratch: &mut Vec<u8>) -> Result<usize> {
        if key.len() <= 9 || key[key.len() - 9] != 0 {
            return Err(Error::Format);
        }
        self.refname.clear();
        self.refname.extend_from_slice(&key[..key.len() - 9]);
        let ts = get_be64(&key[key.len() - 8..]);
        self.update_index = !ts;

        if val_type == 0 {
            self.value = LogValue::Deletion;
            return Ok(0);
        }
        if !matches!(self.value, LogValue::Update(_)) {
            self.value = LogValue::Update(LogUpdate::default());
        }
        let LogValue::Update(u) = &mut self.value else {
            unreachable!("just set")
        };

        if input.len() < 2 * hash_size {
            return Err(Error::Format);
        }
        u.old_hash = [0; HASH_SIZE_MAX];
        u.new_hash = [0; HASH_SIZE_MAX];
        u.old_hash[..hash_size].copy_from_slice(&input[..hash_size]);
        u.new_hash[..hash_size].copy_from_slice(&input[hash_size..2 * hash_size]);
        let mut pos = 2 * hash_size;

        pos += decode_string(scratch, &input[pos..]).ok_or(Error::Format)?;
        u.name.clear();
        u.name.extend_from_slice(scratch);

        pos += decode_string(scratch, &input[pos..]).ok_or(Error::Format)?;
        u.email.clear();
        u.email.extend_from_slice(scratch);

        let (time, n) = get_var_int(&input[pos..]).ok_or(Error::Format)?;
        pos += n;
        u.time = time;
        if input.len() - pos < 2 {
            return Err(Error::Format);
        }
        u.tz_offset = get_be16(&input[pos..]) as i16;
        pos += 2;

        pos += decode_string(scratch, &input[pos..]).ok_or(Error::Format)?;
        u.message.clear();
        u.message.extend_from_slice(scratch);
        Ok(pos)
    }
}

/// `struct reftable_obj_record`: an object ID prefix mapped to the offsets of
/// the ref blocks that mention it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ObjRecord {
    /// Leading bytes of the object ID; the length is constant within a table.
    pub hash_prefix: Vec<u8>,
    /// File offsets of ref blocks.
    pub offsets: Vec<u64>,
}

impl ObjRecord {
    /// `reftable_obj_record_val_type()` (`record.c:519-525`): small offset counts
    /// are stored in the key's extra bits.
    fn val_type(&self) -> u8 {
        let n = self.offsets.len();
        if n > 0 && n < 8 { n as u8 } else { 0 }
    }

    /// `reftable_obj_record_encode()` (`record.c:527-557`).
    fn encode(&self, s: &mut [u8]) -> Result<usize> {
        let mut pos = 0;
        let n = self.offsets.len();
        if n == 0 || n >= 8 {
            pos += put_var_int(s, n as u64)?;
        }
        let Some(&first) = self.offsets.first() else {
            return Ok(pos);
        };
        pos += put_var_int(&mut s[pos..], first)?;
        let mut last = first;
        for &off in &self.offsets[1..] {
            pos += put_var_int(&mut s[pos..], off - last)?;
            last = off;
        }
        Ok(pos)
    }

    /// `reftable_obj_record_decode()` (`record.c:559-614`).
    fn decode(&mut self, key: &[u8], val_type: u8, input: &[u8]) -> Result<usize> {
        self.hash_prefix.clear();
        self.hash_prefix.extend_from_slice(key);
        self.offsets.clear();

        let mut pos = 0;
        let mut count = u64::from(val_type);
        if val_type == 0 {
            let (c, n) = get_var_int(input).ok_or(Error::Format)?;
            count = c;
            pos += n;
        }
        if count == 0 {
            return Ok(pos);
        }

        let (first, n) = get_var_int(&input[pos..]).ok_or(Error::Format)?;
        pos += n;
        self.offsets.push(first);
        let mut last = first;
        for _ in 1..count {
            let (delta, n) = get_var_int(&input[pos..]).ok_or(Error::Format)?;
            pos += n;
            last = delta.wrapping_add(last);
            self.offsets.push(last);
        }
        Ok(pos)
    }
}

/// `struct reftable_index_record`: the last key of a block and its offset.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IndexRecord {
    /// Offset of the indexed block.
    pub offset: u64,
    /// Last key of the indexed block.
    pub last_key: Vec<u8>,
}

/// `struct reftable_record`: a record of any of the four types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    /// `REFTABLE_BLOCK_TYPE_REF`.
    Ref(RefRecord),
    /// `REFTABLE_BLOCK_TYPE_LOG`.
    Log(LogRecord),
    /// `REFTABLE_BLOCK_TYPE_OBJ`.
    Obj(ObjRecord),
    /// `REFTABLE_BLOCK_TYPE_INDEX`.
    Index(IndexRecord),
}

impl Record {
    /// `reftable_record_init()` (`record.c:1306-1322`): an empty record of block type `typ`.
    pub fn new(typ: u8) -> Result<Self> {
        Ok(match typ {
            BLOCK_TYPE_REF => Record::Ref(RefRecord::default()),
            BLOCK_TYPE_LOG => Record::Log(LogRecord::default()),
            BLOCK_TYPE_OBJ => Record::Obj(ObjRecord::default()),
            BLOCK_TYPE_INDEX => Record::Index(IndexRecord::default()),
            _ => return Err(Error::Api),
        })
    }

    /// `reftable_record_type()`: the block type this record belongs in.
    pub fn typ(&self) -> u8 {
        match self {
            Record::Ref(_) => BLOCK_TYPE_REF,
            Record::Log(_) => BLOCK_TYPE_LOG,
            Record::Obj(_) => BLOCK_TYPE_OBJ,
            Record::Index(_) => BLOCK_TYPE_INDEX,
        }
    }

    /// `reftable_record_key()`: write the record's sort key into `dest`.
    pub fn key(&self, dest: &mut Vec<u8>) {
        match self {
            Record::Ref(r) => {
                dest.clear();
                dest.extend_from_slice(&r.refname);
            }
            Record::Log(r) => r.key(dest),
            Record::Obj(r) => {
                dest.clear();
                dest.extend_from_slice(&r.hash_prefix);
            }
            Record::Index(r) => {
                dest.clear();
                dest.extend_from_slice(&r.last_key);
            }
        }
    }

    /// `reftable_record_val_type()`: the three "extra" bits stored with the key.
    pub fn val_type(&self) -> u8 {
        match self {
            Record::Ref(r) => r.value.val_type(),
            Record::Log(r) => u8::from(!r.is_deletion()),
            Record::Obj(r) => r.val_type(),
            Record::Index(_) => 0,
        }
    }

    /// `reftable_record_encode()`: encode the value into `dest`, returning its length.
    pub fn encode(&self, dest: &mut [u8], hash_size: usize) -> Result<usize> {
        match self {
            Record::Ref(r) => r.encode(dest, hash_size),
            Record::Log(r) => r.encode(dest, hash_size),
            Record::Obj(r) => r.encode(dest),
            Record::Index(r) => put_var_int(dest, r.offset),
        }
    }

    /// `reftable_record_decode()`: decode the value that follows `key`.
    pub fn decode(
        &mut self,
        key: &[u8],
        extra: u8,
        src: &[u8],
        hash_size: usize,
        scratch: &mut Vec<u8>,
    ) -> Result<usize> {
        match self {
            Record::Ref(r) => r.decode(key, extra, src, hash_size, scratch),
            Record::Log(r) => r.decode(key, extra, src, hash_size, scratch),
            Record::Obj(r) => r.decode(key, extra, src),
            Record::Index(r) => {
                r.last_key.clear();
                r.last_key.extend_from_slice(key);
                let (offset, n) = get_var_int(src).ok_or(Error::Format)?;
                r.offset = offset;
                Ok(n)
            }
        }
    }

    /// `reftable_record_is_deletion()`.
    pub fn is_deletion(&self) -> bool {
        match self {
            Record::Ref(r) => r.is_deletion(),
            Record::Log(r) => r.is_deletion(),
            Record::Obj(_) | Record::Index(_) => false,
        }
    }

    /// `reftable_record_cmp()`: compare the keys of two records of the same type.
    ///
    /// Log records compare the update index with a proper ordering where C
    /// returns `b - a` truncated to `int` (`record.c:981-995`); the two agree for
    /// every update index a repository will reach.
    pub fn cmp_key(&self, other: &Record) -> Result<Ordering> {
        Ok(match (self, other) {
            (Record::Ref(a), Record::Ref(b)) => a.refname.cmp(&b.refname),
            (Record::Log(a), Record::Log(b)) => a.compare_key(b),
            (Record::Obj(a), Record::Obj(b)) => {
                let n = a.hash_prefix.len().min(b.hash_prefix.len());
                a.hash_prefix[..n]
                    .cmp(&b.hash_prefix[..n])
                    .then_with(|| a.hash_prefix.len().cmp(&b.hash_prefix.len()))
            }
            (Record::Index(a), Record::Index(b)) => a.last_key.cmp(&b.last_key),
            _ => return Err(Error::Api),
        })
    }

    /// The ref record, if this is one.
    pub fn as_ref_record(&self) -> Option<&RefRecord> {
        match self {
            Record::Ref(r) => Some(r),
            _ => None,
        }
    }

    /// The log record, if this is one.
    pub fn as_log_record(&self) -> Option<&LogRecord> {
        match self {
            Record::Log(r) => Some(r),
            _ => None,
        }
    }
}
