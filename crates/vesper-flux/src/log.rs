//! Durable, replayable input log: one frame per epoch in one append-only file.
//!
//! The frame codec and its checksums are the Vesper port's
//! (`vesper/src/log.ves`); this module keeps the file.
//!
//! Layout: a header (`VFLUXIL1`, version, flags, input count, every input
//! width, then a BLAKE3 checksum of those bytes), then frames. A frame is the
//! payload length (u32 LE), the payload (epoch number, then per input the row
//! count and canonical rows: i64 LE cells and an i64 LE weight) and a BLAKE3
//! checksum over length and payload. The epoch number is the frame's position,
//! so it is the source offset that a sink commits with its outputs.
//!
//! `append` writes one frame and syncs the file before returning, so an
//! acknowledged epoch survives a crash. `open` scans every frame: an
//! incomplete or checksum-failing *last* frame is a torn append and is cut
//! off durably (reported in [`LogRecovery`]); a bad frame followed by more
//! bytes is corruption and a typed error. Reads re-verify the checksum and
//! decode with the configured limits, so a corrupted file never yields
//! rows. This is a single-writer file: two processes appending to the same
//! path is not detected. Durability rests on the operating system honouring
//! `File::sync_all`; nothing here is evidence about a particular disk.
use super::TickInput;
use crate::Error;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 8] = b"VFLUXIL1";
const VERSION: u16 = 1;
const SUM: usize = 32;

/// Bounds of one log. Hard caps: 1024 inputs, 64 MiB frames, 2^32 epochs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogLimits {
    pub max_inputs: usize,
    pub max_columns: usize,
    pub max_frame_bytes: usize,
    pub max_rows_per_frame: usize,
    pub max_epochs: u64,
}
impl Default for LogLimits {
    fn default() -> Self {
        Self {
            max_inputs: 64,
            max_columns: 32,
            max_frame_bytes: 1 << 20,
            max_rows_per_frame: 16_384,
            max_epochs: 1 << 20,
        }
    }
}
impl LogLimits {
    fn validate(self) -> Result<(), Error> {
        if self.max_inputs == 0
            || self.max_inputs > 1024
            || self.max_columns > 1024
            || self.max_frame_bytes < 16
            || self.max_frame_bytes > 64 << 20
            || self.max_epochs > 1 << 32
        {
            return Err(Error::Limit("log configuration"));
        }
        Ok(())
    }
}

/// What [`InputLog::open`] found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LogRecovery {
    pub epochs: u64,
    /// Bytes of a torn final append that were cut off.
    pub truncated_bytes: u64,
}

/// An append-only epoch log on disk (see the module docs).
#[derive(Debug)]
pub struct InputLog {
    file: File,
    path: PathBuf,
    widths: Vec<usize>,
    limits: LogLimits,
    /// Offset and payload length of every frame.
    frames: Vec<(u64, u32)>,
    end: u64,
}

fn io(_: std::io::Error) -> Error {
    Error::Storage("io")
}

fn header(widths: &[usize]) -> Result<Vec<u8>, Error> {
    u32::try_from(widths.len()).map_err(|_| Error::Overflow)?;
    for w in widths {
        u32::try_from(*w).map_err(|_| Error::Overflow)?;
    }
    Ok(crate::port::log_header(widths))
}

#[cfg(unix)]
fn sync_dir(path: &Path) -> Result<(), Error> {
    if let Some(parent) = path.parent() {
        let dir = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
        File::open(dir).and_then(|d| d.sync_all()).map_err(io)?;
    }
    Ok(())
}
#[cfg(not(unix))]
fn sync_dir(_: &Path) -> Result<(), Error> {
    Ok(())
}

impl InputLog {
    /// Create a new, empty log for inputs of these widths. Fails if the path exists.
    pub fn create(
        path: impl AsRef<Path>,
        widths: &[usize],
        limits: LogLimits,
    ) -> Result<Self, Error> {
        limits.validate()?;
        if widths.is_empty() || widths.len() > limits.max_inputs {
            return Err(Error::Limit("log inputs"));
        }
        if widths.iter().any(|w| *w > limits.max_columns) {
            return Err(Error::Limit("columns"));
        }
        let path = path.as_ref().to_path_buf();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    Error::Storage("log exists")
                } else {
                    Error::Storage("io")
                }
            })?;
        let h = header(widths)?;
        file.write_all(&h).map_err(io)?;
        file.sync_all().map_err(io)?;
        sync_dir(&path)?;
        Ok(Self {
            file,
            path,
            widths: widths.to_vec(),
            limits,
            frames: Vec::new(),
            end: h.len() as u64,
        })
    }

    /// Open an existing log, verify every frame and cut a torn final append.
    pub fn open(path: impl AsRef<Path>, limits: LogLimits) -> Result<(Self, LogRecovery), Error> {
        limits.validate()?;
        let path = path.as_ref().to_path_buf();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    Error::Storage("missing log")
                } else {
                    Error::Storage("io")
                }
            })?;
        let len = file.metadata().map_err(io)?.len();
        let mut fixed = [0u8; 16];
        file.read_exact(&mut fixed)
            .map_err(|_| Error::Storage("log header"))?;
        if &fixed[..8] != MAGIC
            || u16::from_le_bytes([fixed[8], fixed[9]]) != VERSION
            || u16::from_le_bytes([fixed[10], fixed[11]]) != 0
        {
            return Err(Error::Storage("log header"));
        }
        let count = u32::from_le_bytes(fixed[12..16].try_into().map_err(|_| Error::Overflow)?);
        let count = usize::try_from(count).map_err(|_| Error::Overflow)?;
        if count == 0 || count > limits.max_inputs {
            return Err(Error::Limit("log inputs"));
        }
        let mut rest = vec![0u8; 4 * count + SUM];
        file.read_exact(&mut rest)
            .map_err(|_| Error::Storage("log header"))?;
        let mut widths = Vec::with_capacity(count);
        for chunk in rest[..4 * count].as_chunks::<4>().0 {
            let w = usize::try_from(u32::from_le_bytes(*chunk)).map_err(|_| Error::Overflow)?;
            if w > limits.max_columns {
                return Err(Error::Limit("columns"));
            }
            widths.push(w);
        }
        if header(&widths)? != [&fixed[..], &rest[..]].concat() {
            return Err(Error::Storage("log header"));
        }
        let mut log = Self {
            file,
            path,
            widths,
            limits,
            frames: Vec::new(),
            end: (16 + rest.len()) as u64,
        };
        let mut recovery = LogRecovery::default();
        loop {
            match log.scan_frame(log.end, len)? {
                Scan::Frame(payload_len) => {
                    if log.frames.len() as u64 >= limits.max_epochs {
                        return Err(Error::Limit("log epochs"));
                    }
                    log.frames.push((log.end, payload_len));
                    log.end += 4 + u64::from(payload_len) + SUM as u64;
                }
                Scan::End => break,
                Scan::Torn => {
                    recovery.truncated_bytes = len - log.end;
                    log.file.set_len(log.end).map_err(io)?;
                    log.file.sync_all().map_err(io)?;
                    break;
                }
            }
        }
        recovery.epochs = log.frames.len() as u64;
        Ok((log, recovery))
    }

    fn scan_frame(&mut self, at: u64, len: u64) -> Result<Scan, Error> {
        if at == len {
            return Ok(Scan::End);
        }
        if len - at < 4 {
            return Ok(Scan::Torn);
        }
        let mut l = [0u8; 4];
        self.read_at(at, &mut l)?;
        let payload_len = u32::from_le_bytes(l);
        let total = 4 + u64::from(payload_len) + SUM as u64;
        if payload_len as usize > self.limits.max_frame_bytes {
            // A length field is written whole before its payload, so a torn
            // append keeps a genuine length. An oversized one is a frame
            // written under larger limits, or corruption: neither may be
            // truncated away.
            return Err(Error::Limit("log frame bytes"));
        }
        if len - at < total {
            return Ok(Scan::Torn);
        }
        let mut body = vec![0u8; payload_len as usize + SUM];
        self.read_at(at + 4, &mut body)?;
        let payload = &body[..payload_len as usize];
        if !crate::port::log_frame_ok(&body, payload_len) {
            return if len - at == total {
                Ok(Scan::Torn)
            } else {
                Err(Error::Storage("corrupt log"))
            };
        }
        // A checksummed frame must also decode; otherwise the writer was broken.
        self.decode(payload, self.frames.len() as u64)?;
        Ok(Scan::Frame(payload_len))
    }

    fn read_at(&self, at: u64, buf: &mut [u8]) -> Result<(), Error> {
        let mut f = &self.file;
        f.seek(SeekFrom::Start(at)).map_err(io)?;
        f.read_exact(buf)
            .map_err(|_| Error::Storage("truncated log"))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn widths(&self) -> &[usize] {
        &self.widths
    }
    /// Durable epochs.
    pub fn len(&self) -> u64 {
        self.frames.len() as u64
    }
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Append one epoch and sync it. Returns the new epoch's number.
    pub fn append(&mut self, input: &TickInput) -> Result<u64, Error> {
        let epoch = self.len();
        if epoch >= self.limits.max_epochs {
            return Err(Error::Limit("log epochs"));
        }
        if input.deltas.len() != self.widths.len() {
            return Err(Error::InputCount);
        }
        let frame = crate::port::log_frame(epoch, &input.deltas, &self.widths, self.limits)?;
        let mut f = &self.file;
        let written = f
            .seek(SeekFrom::Start(self.end))
            .and_then(|_| f.write_all(&frame))
            .and_then(|()| self.file.sync_all());
        if written.is_err() {
            // Leave the file as the last durable state; a reopen cuts any tail.
            let _ = self.file.set_len(self.end);
            return Err(Error::Storage("io"));
        }
        let len = (frame.len() - 4 - SUM) as u32;
        self.frames.push((self.end, len));
        self.end += frame.len() as u64;
        Ok(epoch)
    }

    /// Read and verify one epoch.
    pub fn read(&self, epoch: u64) -> Result<TickInput, Error> {
        let i = usize::try_from(epoch).map_err(|_| Error::Overflow)?;
        let (at, len) = *self
            .frames
            .get(i)
            .ok_or(Error::Storage("epoch past log end"))?;
        let mut body = vec![0u8; len as usize + SUM];
        self.read_at(at + 4, &mut body)?;
        if !crate::port::log_frame_ok(&body, len) {
            return Err(Error::Storage("corrupt log"));
        }
        self.decode(&body[..len as usize], epoch)
    }

    fn decode(&self, payload: &[u8], epoch: u64) -> Result<TickInput, Error> {
        let deltas = crate::port::log_decode(payload, epoch, &self.widths, self.limits)?;
        Ok(TickInput { deltas })
    }
}

enum Scan {
    Frame(u32),
    End,
    Torn,
}
