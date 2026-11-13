use std::{io, io::Write};

use crate::{
    client::{MessageKind, WriteMode, blocking_io::ExtendedBufRead},
    packetline::blocking_io::{Writer, encode},
};

/// The `struct strbuf req_buf` a request is built in before it goes out whole.
///
/// git writes most requests whole rather than one pkt-line at a time: `find_common()` collects its
/// wants, shallow lines and `deepen*` arguments in `req_buf` and hands the lot to
/// `send_request()`, one `write_in_full()` (fetch-pack.c:254-264, :361-459),
/// `send_fetch_request()` does the same for a v2 `fetch` (fetch-pack.c:1384-1445),
/// `write_command_and_capabilities()` for every v2 command it opens (connect.c:712-750) and
/// `send_pack()` for its commands, ahead of a `packet_flush()` of their own
/// (send-pack.c:486-694). `get_remote_refs()` is the exception:
/// it writes `ls-refs` one `packet_write_fmt()` per line (connect.c:583-606), which a caller
/// gets by flushing after each line. A server that dies over the first line it reads
/// (`deepen-not is not a ref`) has then been given the whole request already, so
/// the client learns of the death from the read that follows,
/// `the remote end hung up unexpectedly`, and never from a write into the closed
/// pipe. Packet lines are collected here until a flush sends them in one
/// `write_all()`; once [`RequestWriter::into_parts()`] gives the writer away the
/// collected lines go out ahead of the first write and everything after passes
/// straight through, as a pack follows `send_pack()`s commands.
struct RequestBuf<W: io::Write> {
    req_buf: Vec<u8>,
    out: W,
    passthrough: bool,
}

impl<W: io::Write> RequestBuf<W> {
    /// `send_request()`: `write_in_full(fd, buf->buf, buf->len)`, then `strbuf_setlen(&req_buf, 0)`.
    fn send_request(&mut self) -> io::Result<()> {
        if !self.req_buf.is_empty() {
            let buf = std::mem::take(&mut self.req_buf);
            self.out.write_all(&buf)?;
        }
        Ok(())
    }
}

impl<W: io::Write> io::Write for RequestBuf<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.passthrough {
            self.send_request()?;
            return self.out.write(buf);
        }
        self.req_buf.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.send_request()?;
        self.out.flush()
    }
}

impl<W: io::Write> Drop for RequestBuf<W> {
    /// A writer dropped with lines still collected sends them, as `BufWriter` would.
    fn drop(&mut self) {
        let _ = self.send_request();
    }
}

/// A [`Write`][io::Write] implementation optimized for writing packet lines.
/// A type implementing `Write` for packet lines, which when done can be transformed into a `Read` for
/// obtaining the response.
pub struct RequestWriter<'a> {
    on_into_read: MessageKind,
    writer: Writer<RequestBuf<Box<dyn io::Write + 'a>>>,
    reader: Box<dyn ExtendedBufRead<'a> + Unpin + 'a>,
    trace: bool,
}

impl io::Write for RequestWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.trace {
            #[allow(unused_imports)]
            use bstr::ByteSlice;
            gix_features::trace::trace!(">> {}", buf.as_bstr());
        }
        self.writer.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

/// methods with bonds to IO
impl<'a> RequestWriter<'a> {
    /// Create a new instance from a `writer` (commonly a socket), a `reader` into which to transform once the
    /// writes are finished, along with configuration for the `write_mode` and information about which message to write
    /// when this instance is converted into a `reader` to read the request's response.
    /// If `trace` is true, `gix_trace` will be used on every written message or data.
    pub fn new_from_bufread<W: io::Write + 'a>(
        writer: W,
        reader: Box<dyn ExtendedBufRead<'a> + Unpin + 'a>,
        write_mode: WriteMode,
        on_into_read: MessageKind,
        trace: bool,
    ) -> Self {
        let mut writer = Writer::new(RequestBuf {
            req_buf: Vec::new(),
            out: Box::new(writer) as Box<dyn io::Write>,
            passthrough: false,
        });
        match write_mode {
            WriteMode::Binary => writer.enable_binary_mode(),
            WriteMode::OneLfTerminatedLinePerWriteCall => writer.enable_text_mode(),
        }
        RequestWriter {
            on_into_read,
            writer,
            reader,
            trace,
        }
    }

    /// Write the given message as packet line.
    pub fn write_message(&mut self, message: MessageKind) -> io::Result<()> {
        match message {
            MessageKind::Flush => {
                if self.trace {
                    gix_features::trace::trace!(">> FLUSH");
                }
                encode::write_packet_line(&gix_packetline::PacketLineRef::Flush, self.writer.inner_mut())
            }
            MessageKind::Delimiter => {
                if self.trace {
                    gix_features::trace::trace!(">> DELIM");
                }
                encode::write_packet_line(&gix_packetline::PacketLineRef::Delimiter, self.writer.inner_mut())
            }
            MessageKind::ResponseEnd => {
                if self.trace {
                    gix_features::trace::trace!(">> RESPONSE_END");
                }
                encode::write_packet_line(&gix_packetline::PacketLineRef::ResponseEnd, self.writer.inner_mut())
            }
            MessageKind::Text(t) => {
                if self.trace {
                    #[allow(unused_imports)]
                    use bstr::ByteSlice;
                    gix_features::trace::trace!(">> {}", t.as_bstr());
                }
                encode::write_text(&gix_packetline::TextRef::from(t), self.writer.inner_mut())
            }
        }
        .map(|_| ())
    }

    /// Discard the ability to write and turn this instance into the reader for obtaining the other side's response.
    ///
    /// Doing so will also write the message type this instance was initialized with.
    pub fn into_read(mut self) -> std::io::Result<Box<dyn ExtendedBufRead<'a> + Unpin + 'a>> {
        self.write_message(self.on_into_read)?;
        self.writer.inner_mut().flush()?;
        Ok(self.reader)
    }

    /// Dissolve this instance into its write and read handles without any message-writing side-effect as in [`RequestWriter::into_read()`].
    ///
    /// Furthermore, the writer will not encode everything it writes as packetlines, but write everything verbatim into the
    /// underlying channel.
    ///
    /// # Note
    ///
    /// It's of utmost importance to drop the request writer before reading the response as these might be inter-dependent, depending on
    /// the underlying transport mechanism. Failure to do so may result in a deadlock depending on how the write and read mechanism
    /// is implemented.
    pub fn into_parts(self) -> (Box<dyn io::Write + 'a>, Box<dyn ExtendedBufRead<'a> + Unpin + 'a>) {
        let mut writer = self.writer.into_inner();
        writer.passthrough = true;
        (Box::new(writer), self.reader)
    }
}
