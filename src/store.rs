//! The unit store: method bodies kept in a file instead of on the heap.
//!
//! An app's IR is almost all method bodies, and the whole-program
//! fixpoint retypes only the classes whose registry reads moved
//! (`analyze::incremental`). With `ROUNDHOUSE_STORE=spill`, the library
//! pass writes each class's bodies here as soon as it has typed them and
//! keeps only a [`Handle`] in their place ([`crate::body::Body`]). A body
//! that anything reads again is decoded from the file on that read, so no
//! reader has to know where a body lives, and what the compiler computes
//! does not depend on it: the store changes memory and time, not output.
//!
//! Records are JSON (`serde_json`, already a dependency, and the only
//! format here that round-trips the IR's optional fields), appended to
//! one file that is unlinked as soon as it is created and so disappears
//! with the process. `ROUNDHOUSE_STORE_DIR` chooses its directory (the
//! system temp directory otherwise); a RAM-backed filesystem there would
//! keep the bodies in memory after all, so the store refuses one.
//! `ROUNDHOUSE_STORE=check` also decodes every record as it is written and
//! fails if it does not encode back to the same bytes.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use crate::expr::Expr;

/// Where one record is in the store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Handle {
    offset: u64,
    len: u32,
}

pub struct Store {
    file: File,
    end: AtomicU64,
    check: bool,
    written: AtomicU64,
    read: AtomicU64,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Store")
            .field("bytes", &self.end.load(Ordering::Relaxed))
            .finish()
    }
}

/// How `ROUNDHOUSE_STORE` asks bodies to be kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// On the heap, as without a store.
    Resident,
    /// In the store between the passes that type them.
    Spill,
    /// As `Spill`, and every record is checked as it is written.
    Check,
}

impl Mode {
    pub fn from_env() -> Self {
        match std::env::var("ROUNDHOUSE_STORE").as_deref() {
            Ok("spill") => Mode::Spill,
            Ok("check") => Mode::Check,
            _ => Mode::Resident,
        }
    }
}

/// The store for this process, when `ROUNDHOUSE_STORE` asks for one.
pub fn from_env() -> Option<Arc<Store>> {
    let mode = Mode::from_env();
    if mode == Mode::Resident {
        return None;
    }
    let directory = std::env::var_os("ROUNDHOUSE_STORE_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    match Store::create(&directory, mode == Mode::Check) {
        Ok(store) => Some(Arc::new(store)),
        Err(error) => panic!("ROUNDHOUSE_STORE: cannot create the unit store in {}: {error}", directory.display()),
    }
}

/// Whether `directory` is on tmpfs or ramfs: the filesystem of the
/// longest mount point in `/proc/self/mounts` that contains it.
#[cfg(target_os = "linux")]
fn is_ram_backed(directory: &Path) -> bool {
    let Ok(directory) = directory.canonicalize() else { return false };
    let Ok(mounts) = std::fs::read_to_string("/proc/self/mounts") else { return false };
    mounts
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let _device = fields.next()?;
            let point = fields.next()?.replace("\\040", " ");
            let kind = fields.next()?;
            directory.starts_with(&point).then(|| (point.len(), kind == "tmpfs" || kind == "ramfs"))
        })
        .max_by_key(|(length, _)| *length)
        .is_some_and(|(_, ram)| ram)
}

#[cfg(not(target_os = "linux"))]
fn is_ram_backed(_directory: &Path) -> bool {
    false
}

#[cfg(unix)]
fn write_at(file: &File, bytes: &[u8], offset: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, bytes, offset)
}

#[cfg(unix)]
fn read_at(file: &File, bytes: &mut [u8], offset: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, bytes, offset)
}

#[cfg(not(unix))]
fn write_at(_file: &File, _bytes: &[u8], _offset: u64) -> std::io::Result<()> {
    Err(std::io::Error::other("the unit store needs positional file I/O"))
}

#[cfg(not(unix))]
fn read_at(_file: &File, _bytes: &mut [u8], _offset: u64) -> std::io::Result<()> {
    Err(std::io::Error::other("the unit store needs positional file I/O"))
}

impl Store {
    /// A new, empty store in `directory`, refused on a RAM-backed one.
    pub fn create(directory: &Path, check: bool) -> std::io::Result<Self> {
        if cfg!(not(unix)) {
            return Err(std::io::Error::other("the unit store needs positional file I/O"));
        }
        if is_ram_backed(directory) {
            return Err(std::io::Error::other(
                "the directory is RAM-backed (tmpfs), so stored bodies would stay in memory; set ROUNDHOUSE_STORE_DIR to a directory on disk",
            ));
        }
        // Unique within the process too: two stores created at once (the
        // tests below run in parallel) must not race for one name.
        static CREATED: AtomicU64 = AtomicU64::new(0);
        let path = directory.join(format!("roundhouse-store-{}-{}", std::process::id(), CREATED.fetch_add(1, Ordering::Relaxed)));
        let file = std::fs::OpenOptions::new().read(true).write(true).create_new(true).open(&path)?;
        std::fs::remove_file(&path)?;
        Ok(Store {
            file,
            end: AtomicU64::new(0),
            check,
            written: AtomicU64::new(0),
            read: AtomicU64::new(0),
        })
    }

    /// Append `expr`, or `None` when it has no JSON form (a non-finite
    /// float literal), and so stays on the heap.
    pub fn write(&self, expr: &Expr) -> Option<Handle> {
        let bytes = serde_json::to_vec(expr).ok()?;
        let len = u32::try_from(bytes.len()).ok()?;
        if self.check {
            let decoded: Expr = serde_json::from_slice(&bytes).expect("ROUNDHOUSE_STORE=check: a stored body does not decode");
            let again = serde_json::to_vec(&decoded).expect("ROUNDHOUSE_STORE=check: a decoded body does not encode");
            assert!(again == bytes, "ROUNDHOUSE_STORE=check: a stored body does not round-trip");
        }
        let offset = self.end.fetch_add(u64::from(len), Ordering::Relaxed);
        if let Err(error) = write_at(&self.file, &bytes, offset) {
            panic!("ROUNDHOUSE_STORE: writing the unit store failed: {error}");
        }
        self.written.fetch_add(u64::from(len), Ordering::Relaxed);
        Some(Handle { offset, len })
    }

    /// The body `handle` names.
    pub fn read(&self, handle: Handle) -> Expr {
        let mut bytes = vec![0u8; handle.len as usize];
        if let Err(error) = read_at(&self.file, &mut bytes, handle.offset) {
            panic!("ROUNDHOUSE_STORE: reading the unit store failed: {error}");
        }
        self.read.fetch_add(u64::from(handle.len), Ordering::Relaxed);
        serde_json::from_slice(&bytes).unwrap_or_else(|error| panic!("ROUNDHOUSE_STORE: a stored body does not decode: {error}"))
    }

    /// One line for `ROUNDHOUSE_TIMINGS`: bytes written and read so far.
    pub fn summary(&self) -> String {
        format!(
            "unit store: {:.1} MB written, {:.1} MB read",
            self.written.load(Ordering::Relaxed) as f64 / 1e6,
            self.read.load(Ordering::Relaxed) as f64 / 1e6,
        )
    }
}

/// A stored body and, once something has read it, its decoded tree.
#[derive(Debug)]
pub struct Stored {
    store: Arc<Store>,
    handle: Handle,
    decoded: OnceLock<Expr>,
}

impl Stored {
    pub fn new(store: Arc<Store>, handle: Handle) -> Self {
        Stored { store, handle, decoded: OnceLock::new() }
    }

    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    /// The tree, decoded on the first read.
    pub fn get(&self) -> &Expr {
        self.decoded.get_or_init(|| {
            self.store.read(self.handle)
        })
    }

    /// Run `read` over the tree without keeping a decoded copy: the one
    /// already decoded, or a temporary one.
    pub fn with<R>(&self, read: impl FnOnce(&Expr) -> R) -> R {
        match self.decoded.get() {
            Some(expr) => read(expr),
            None => read(&self.store.read(self.handle)),
        }
    }

    /// The tree, owned: the decoded copy when there is one.
    pub fn take(self) -> Expr {
        match self.decoded.into_inner() {
            Some(expr) => expr,
            None => self.store.read(self.handle),
        }
    }

    /// Forget the decoded copy; the next read decodes again.
    pub fn evict(&mut self) {
        self.decoded = OnceLock::new();
    }
}

impl Clone for Stored {
    fn clone(&self) -> Self {
        Stored { store: Arc::clone(&self.store), handle: self.handle, decoded: OnceLock::new() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::body::Body;

    /// A store in the system temp directory, or `None` where that is
    /// RAM-backed (the store refuses it, which is what it should do).
    fn store(check: bool) -> Option<Arc<Store>> {
        Store::create(&std::env::temp_dir(), check).ok().map(Arc::new)
    }

    fn body() -> Body {
        let method = crate::runtime_src::parse_methods("def total(items)\n  sum = 0\n  items.each { |item| sum += item * 2 }\n  \"#{sum} items\"\nend\n")
            .expect("parses")
            .pop()
            .expect("one method");
        method.body
    }

    #[test]
    fn a_spilled_body_reads_as_the_tree_it_was() {
        let Some(store) = store(true) else { return };
        let original = body();
        let mut spilled = original.clone();
        spilled.spill(&store);
        assert!(spilled.resident().is_none(), "a spilled body leaves the heap");
        assert_eq!(serde_json::to_vec(&*spilled).unwrap(), serde_json::to_vec(&*original).unwrap());
        assert_eq!(spilled.with_expr(|expr| expr.span), original.span);
    }

    #[test]
    fn a_float_literal_reads_back_as_the_same_number() {
        let Some(store) = store(false) else { return };
        // Without serde_json's `float_roundtrip`, this decodes one unit in
        // the last place low.
        let value = 116.47000000812113_f64;
        let mut spilled = Body::new(Expr::new(
            crate::span::Span::synthetic(),
            crate::expr::ExprNode::Lit { value: crate::expr::Literal::Float { value } },
        ));
        spilled.spill(&store);
        let read = spilled.with_expr(|expr| match &*expr.node {
            crate::expr::ExprNode::Lit { value: crate::expr::Literal::Float { value } } => *value,
            other => panic!("not a float literal: {other:?}"),
        });
        assert_eq!(read.to_bits(), value.to_bits(), "{read} read back for {value}");
    }

    #[test]
    fn a_body_read_back_is_not_written_again() {
        let Some(store) = store(false) else { return };
        let mut spilled = body();
        spilled.spill(&store);
        let written = store.written.load(Ordering::Relaxed);
        let _ = spilled.span;
        spilled.spill(&store);
        assert_eq!(store.written.load(Ordering::Relaxed), written, "only read, so the record stands");
        spilled.span = crate::span::Span::synthetic();
        assert!(spilled.resident().is_some(), "a mutable borrow brings the body back to the heap");
        spilled.spill(&store);
        assert!(store.written.load(Ordering::Relaxed) > written, "a changed body is written again");
    }
}
