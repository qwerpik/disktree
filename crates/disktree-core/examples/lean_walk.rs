//! Floor test: how fast can a bare walk list and stat a tree on this machine?
//! `lean_walk ROOT THREADS MODE` with MODE `rel` (openat from the parent's
//! fd) or `abs` (open by full path). Stays on no device boundary; counts only.
#![allow(clippy::pedantic, clippy::all)]

#[cfg(target_os = "linux")]
mod imp {
    use std::ffi::{CStr, CString};
    use std::mem::MaybeUninit;
    use std::os::fd::OwnedFd;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    use std::time::Instant;

    use rustix::fs::{
        AtFlags, CWD, FileType, Mode, OFlags, RawDir, StatxFlags, openat, statx,
    };

    static FILES: AtomicU64 = AtomicU64::new(0);
    static DIRS: AtomicU64 = AtomicU64::new(0);
    static BLOCKS: AtomicU64 = AtomicU64::new(0);

    const SKIP: &[&str] = &["/proc", "/sys", "/dev", "/run", "/tmp", "/boot"];

    fn walk<'s>(
        scope: &rayon::Scope<'s>,
        fd: Arc<OwnedFd>,
        path: CString,
        rel: bool,
    ) {
        let mut buf = vec![MaybeUninit::<u8>::uninit(); 64 * 1024];
        let mut dir = RawDir::new(&*fd, &mut buf);
        let (mut files, mut blocks) = (0u64, 0u64);
        let mask = StatxFlags::TYPE
            | StatxFlags::SIZE
            | StatxFlags::BLOCKS
            | StatxFlags::MTIME
            | StatxFlags::NLINK
            | StatxFlags::INO;
        while let Some(entry) = dir.next() {
            let Ok(entry) = entry else { break };
            let name: &CStr = entry.file_name();
            let bytes = name.to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            if entry.file_type() == FileType::Directory {
                let mut child = path.as_bytes().to_vec();
                if child.last() != Some(&b'/') {
                    child.push(b'/');
                }
                child.extend_from_slice(bytes);
                let child = CString::new(child).unwrap();
                if SKIP.iter().any(|s| s.as_bytes() == child.as_bytes()) {
                    continue;
                }
                let flags = OFlags::RDONLY
                    | OFlags::DIRECTORY
                    | OFlags::NOFOLLOW
                    | OFlags::CLOEXEC;
                let opened = if rel {
                    openat(&*fd, name, flags, Mode::empty())
                } else {
                    openat(CWD, child.as_c_str(), flags, Mode::empty())
                };
                if let Ok(child_fd) = opened {
                    DIRS.fetch_add(1, Relaxed);
                    scope.spawn(move |s| {
                        walk(s, Arc::new(child_fd), child, rel)
                    });
                }
            } else if let Ok(st) =
                statx(&*fd, name, AtFlags::SYMLINK_NOFOLLOW, mask)
            {
                files += 1;
                blocks += st.stx_blocks;
            }
        }
        FILES.fetch_add(files, Relaxed);
        BLOCKS.fetch_add(blocks, Relaxed);
    }

    pub fn main() {
        let mut args = std::env::args().skip(1);
        let root = args.next().unwrap_or_else(|| "/".into());
        let threads: usize = args.next().map_or(12, |t| t.parse().unwrap());
        let rel = args.next().as_deref() != Some("abs");
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap();
        let start = Instant::now();
        let path = CString::new(root.clone()).unwrap();
        let fd = openat(
            CWD,
            path.as_c_str(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .unwrap();
        pool.scope(|s| walk(s, Arc::new(fd), path, rel));
        println!(
            "{root} threads={threads} rel={rel}: {:.3}s files={} dirs={} bytes={:.1}G",
            start.elapsed().as_secs_f64(),
            FILES.load(Relaxed),
            DIRS.load(Relaxed),
            BLOCKS.load(Relaxed) as f64 * 512.0 / 1e9
        );
    }
}

fn main() {
    #[cfg(target_os = "linux")]
    imp::main();
}
