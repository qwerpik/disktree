//! Directory listing for the walk on Linux: `getdents64` into a reused
//! buffer, then one `statx` per entry relative to the open directory, asking
//! only for what a measurement uses.
//!
//! `std::fs::read_dir` costs three allocations an entry (its own copy of
//! the name, an `OsString` for `file_name`, and the tree's `Box<str>`) and
//! a `statx` for every field the kernel has. Here the name is decoded once,
//! straight into the tree's `Box<str>`, and the kernel fills six fields. On
//! a warm 3-million-file disk the scan went from 0.97 s to 0.93 s; most of
//! what is left is the kernel's own `statx` work (`examples/lean_walk.rs`,
//! which does nothing else, takes 0.78 s).
//!
//! A directory is read whole before the walk sees any of it: the buffer and
//! the descriptor are borrowed by the kernel's iterator, and an owned list
//! is what the walk keeps anyway.

use std::cell::RefCell;
use std::ffi::{CStr, OsStr};
use std::io;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustix::fs::{
    AtFlags, FileType, Mode, OFlags, RawDir, Statx, StatxFlags, makedev,
};

/// Bytes of `getdents64` output per call: room for about a thousand
/// typical entries, so most directories are one call.
const BUFFER_BYTES: usize = 64 * 1024;

thread_local! {
    /// One listing buffer per worker thread, reused for every directory.
    static BUFFER: RefCell<Vec<MaybeUninit<u8>>> =
        RefCell::new(vec![MaybeUninit::uninit(); BUFFER_BYTES]);
}

/// What the walk needs from `statx`, and nothing more.
const WANTED: StatxFlags = StatxFlags::TYPE
    .union(StatxFlags::NLINK)
    .union(StatxFlags::INO)
    .union(StatxFlags::SIZE)
    .union(StatxFlags::BLOCKS)
    .union(StatxFlags::MTIME);

/// What an entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Directory,
    Link,
    File,
    /// A FIFO, a socket or a device node.
    Other,
}

/// What `statx` said about a leaf.
#[derive(Clone, Copy, Debug)]
pub struct Stat {
    pub apparent: u64,
    pub allocated: u64,
    pub modified: i64,
    pub device: u64,
    pub inode: u64,
    pub links: u32,
}

impl Stat {
    fn of(stat: &Statx) -> Self {
        Self {
            apparent: stat.stx_size,
            allocated: stat.stx_blocks.saturating_mul(512),
            // Before 1970 reads as 0, as `SystemTime` arithmetic does.
            modified: stat.stx_mtime.tv_sec.max(0),
            device: makedev(stat.stx_dev_major, stat.stx_dev_minor),
            inode: stat.stx_ino,
            links: stat.stx_nlink,
        }
    }
}

/// One directory entry, with what a measurement needs.
#[derive(Debug)]
pub struct Entry {
    name: Box<str>,
    /// The name as the file system spells it, only when `name` could not:
    /// a name that is not UTF-8 is shown lossy but must still open.
    exact: Option<Box<[u8]>>,
    kind: Kind,
    /// A leaf's `statx`, read while the directory was open. Directories
    /// and links are not asked: the walk opens one and reads the other by
    /// path.
    stat: Option<Result<Stat, io::Error>>,
    /// The directory listed, kept by directory entries only, for the rare
    /// walk that must ask what device one is on.
    parent: Option<Arc<Path>>,
}

impl Entry {
    /// The entry's path inside `dir`, the directory it was listed from.
    pub fn path(&self, dir: &Path) -> PathBuf {
        dir.join(self.file_name())
    }

    pub fn file_name(&self) -> &OsStr {
        OsStr::from_bytes(self.exact.as_deref().unwrap_or(self.name.as_bytes()))
    }

    /// The name as text; see [`Self::file_name`] for what it may lose.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The name, moved out: the tree keeps it, so nothing copies it.
    pub fn take_name(&mut self) -> Box<str> {
        std::mem::take(&mut self.name)
    }

    pub const fn kind(&self) -> Kind {
        self.kind
    }

    /// The leaf's `statx`. A directory or link has none: asking is a bug,
    /// answered with an error rather than a wrong size.
    pub fn stat(&self) -> io::Result<Stat> {
        match &self.stat {
            Some(Ok(stat)) => Ok(*stat),
            Some(Err(error)) => Err(error.raw_os_error().map_or_else(
                || io::Error::new(error.kind(), error.to_string()),
                io::Error::from_raw_os_error,
            )),
            None => Err(io::Error::other("no statx for this entry")),
        }
    }

    /// The device a directory entry is on, read by path when asked.
    pub fn device(&self) -> io::Result<u64> {
        use std::os::unix::fs::MetadataExt as _;
        let parent = self
            .parent
            .as_deref()
            .ok_or_else(|| io::Error::other("not a directory entry"))?;
        std::fs::symlink_metadata(self.path(parent)).map(|meta| meta.dev())
    }
}

/// List `dir` whole, with a `statx` for every leaf in it.
pub fn read_dir(
    dir: &Path,
) -> io::Result<std::vec::IntoIter<io::Result<Entry>>> {
    let fd = rustix::fs::open(
        dir,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let mut entries = Vec::new();
    let mut parent: Option<Arc<Path>> = None;
    BUFFER.with_borrow_mut(|buffer| {
        let mut raw = RawDir::new(&fd, buffer);
        while let Some(next) = raw.next() {
            let listed = match next {
                Ok(listed) => listed,
                Err(error) => {
                    entries.push(Err(error.into()));
                    break;
                }
            };
            let name = listed.file_name();
            if matches!(name.to_bytes(), b"." | b"..") {
                continue;
            }
            let mut stat = None;
            let kind = match listed.file_type() {
                FileType::Directory => Kind::Directory,
                FileType::Symlink => Kind::Link,
                FileType::RegularFile => Kind::File,
                FileType::Unknown => {
                    // A file system that does not fill `d_type`: the
                    // `statx` the leaf needs anyway says what it is.
                    let asked = statx(&fd, name);
                    let kind = asked.as_ref().map_or(Kind::Other, |stat| {
                        kind_of(FileType::from_raw_mode(stat.stx_mode.into()))
                    });
                    if !matches!(kind, Kind::Directory | Kind::Link) {
                        stat = Some(asked.map(|stat| Stat::of(&stat)));
                    }
                    kind
                }
                _ => Kind::Other,
            };
            if stat.is_none() && matches!(kind, Kind::File | Kind::Other) {
                stat = Some(statx(&fd, name).map(|stat| Stat::of(&stat)));
            }
            let (name, exact) = decode(name);
            let parent = (kind == Kind::Directory).then(|| {
                Arc::clone(parent.get_or_insert_with(|| Arc::from(dir)))
            });
            entries.push(Ok(Entry {
                name,
                exact,
                kind,
                stat,
                parent,
            }));
        }
    });
    Ok(entries.into_iter())
}

fn statx(fd: &rustix::fd::OwnedFd, name: &CStr) -> io::Result<Statx> {
    rustix::fs::statx(fd, name, AtFlags::SYMLINK_NOFOLLOW, WANTED)
        .map_err(io::Error::from)
}

const fn kind_of(file_type: FileType) -> Kind {
    match file_type {
        FileType::Directory => Kind::Directory,
        FileType::Symlink => Kind::Link,
        FileType::RegularFile => Kind::File,
        _ => Kind::Other,
    }
}

/// The name for the tree, and the exact bytes when they are not UTF-8.
fn decode(name: &CStr) -> (Box<str>, Option<Box<[u8]>>) {
    let bytes = name.to_bytes();
    match std::str::from_utf8(bytes) {
        Ok(text) => (text.into(), None),
        Err(_) => (
            String::from_utf8_lossy(bytes).into_owned().into_boxed_str(),
            Some(bytes.into()),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_kinds_sizes_and_exact_names() {
        use std::os::unix::ffi::OsStrExt as _;
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("file"), vec![7_u8; 10_000])
            .expect("file");
        std::fs::create_dir(dir.path().join("sub")).expect("dir");
        std::os::unix::fs::symlink("file", dir.path().join("link"))
            .expect("link");
        let odd = OsStr::from_bytes(b"odd\xff");
        std::fs::write(dir.path().join(odd), b"x").expect("odd");

        let mut entries: Vec<Entry> = read_dir(dir.path())
            .expect("list")
            .map(|entry| entry.expect("entry"))
            .collect();
        entries.sort_by(|a, b| a.file_name().cmp(b.file_name()));
        let names: Vec<_> =
            entries.iter().map(|e| (e.file_name(), e.kind())).collect();
        assert_eq!(
            names,
            [
                (OsStr::new("file"), Kind::File),
                (OsStr::new("link"), Kind::Link),
                (odd, Kind::File),
                (OsStr::new("sub"), Kind::Directory),
            ]
        );
        let file = entries[0].stat().expect("stat");
        assert_eq!(file.apparent, 10_000);
        assert_eq!(file.links, 1);
        assert!(entries[1].stat().is_err());
        assert!(entries[3].device().is_ok());
        assert_eq!(entries[2].name(), "odd\u{fffd}");
        assert!(entries[2].path(dir.path()).exists());
    }
}
