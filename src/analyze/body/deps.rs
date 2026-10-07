//! What typing a body read from the class registry.
//!
//! The whole-program fixpoint re-types every body each round against
//! the registry the round before refined. A body whose registry reads
//! all answer the same as when it was last typed is typed to the same
//! tree again, so the incremental library pass skips it. These are the
//! reads: [`Classes`] and [`ClassRef`] stand where `BodyTyper` used to
//! hold the bare map and note every lookup in a [`DepRecorder`] when
//! one is attached.
//!
//! A read is kept as a 64-bit hash of what was read ([`DepKey`]). Two
//! different reads that collide only make a body look stale when it is
//! not, which costs a re-type and never a missed change: the same read
//! always hashes the same.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use rubydex::model::ids::DeclarationId;

use crate::ident::{ClassId, Symbol};
use crate::ty::Ty;

use super::ClassInfo;

/// One registry read.
#[derive(Clone, Copy)]
pub(crate) enum DepKey<'k> {
    /// The class's entry: whether it exists, and every field other than
    /// its two method tables.
    Class(&'k ClassId),
    /// One instance-method entry, present or absent.
    Instance(&'k ClassId, &'k Symbol),
    /// One class-method entry, present or absent.
    ClassMethod(&'k ClassId, &'k Symbol),
    /// The whole instance-method table (an iteration over it).
    AllInstance(&'k ClassId),
    /// The call-site parameter types inferred for a method.
    Params(&'k ClassId, &'k Symbol),
    /// The inferred value of a source constant.
    Constant(&'k DeclarationId),
    /// The app-wide bare-name constant value (`ConstScope::get_global`).
    ConstName(&'k Symbol),
}

/// Multiply-rotate hasher (the `FxHash` mix); the keys are short and
/// collisions are harmless (see the module doc).
#[derive(Default)]
pub(crate) struct Mix(u64);

impl Hasher for Mix {
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut word = [0u8; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            self.write_u64(u64::from_le_bytes(word));
        }
        self.write_u64(bytes.len() as u64);
    }

    fn write_u64(&mut self, i: u64) {
        self.0 = (self.0.rotate_left(5) ^ i).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }

    fn write_u32(&mut self, i: u32) {
        self.write_u64(u64::from(i));
    }

    fn write_u8(&mut self, i: u8) {
        self.write_u64(u64::from(i));
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

impl DepKey<'_> {
    pub(crate) fn hash64(self) -> u64 {
        let mut h = Mix::default();
        match self {
            DepKey::Class(c) => {
                h.write_u8(1);
                h.write(c.0.as_str().as_bytes());
            }
            DepKey::Instance(c, m) => {
                h.write_u8(2);
                h.write(c.0.as_str().as_bytes());
                h.write(m.as_str().as_bytes());
            }
            DepKey::ClassMethod(c, m) => {
                h.write_u8(3);
                h.write(c.0.as_str().as_bytes());
                h.write(m.as_str().as_bytes());
            }
            DepKey::AllInstance(c) => {
                h.write_u8(4);
                h.write(c.0.as_str().as_bytes());
            }
            DepKey::Params(c, m) => {
                h.write_u8(5);
                h.write(c.0.as_str().as_bytes());
                h.write(m.as_str().as_bytes());
            }
            DepKey::Constant(id) => {
                h.write_u8(6);
                id.hash(&mut h);
            }
            DepKey::ConstName(name) => {
                h.write_u8(7);
                h.write(name.as_str().as_bytes());
            }
        }
        h.finish()
    }
}

/// The reads one typing unit made, as [`DepKey::hash64`] values.
#[derive(Default)]
pub(crate) struct DepRecorder(RefCell<Vec<u64>>);

impl DepRecorder {
    pub(crate) fn note(&self, key: DepKey<'_>) {
        self.0.borrow_mut().push(key.hash64());
    }

    /// The distinct reads, sorted.
    pub(crate) fn take(&self) -> Vec<u64> {
        let mut v = std::mem::take(&mut *self.0.borrow_mut());
        v.sort_unstable();
        v.dedup();
        v
    }
}

/// The class registry as `BodyTyper` reads it.
#[derive(Clone, Copy)]
pub(crate) struct Classes<'a> {
    map: &'a HashMap<ClassId, ClassInfo>,
    deps: Option<&'a DepRecorder>,
}

impl<'a> Classes<'a> {
    pub(crate) fn new(map: &'a HashMap<ClassId, ClassInfo>, deps: Option<&'a DepRecorder>) -> Self {
        Self { map, deps }
    }

    fn note(&self, key: DepKey<'_>) {
        if let Some(deps) = self.deps {
            deps.note(key);
        }
    }

    pub(crate) fn get(&self, id: &ClassId) -> Option<ClassRef<'a>> {
        self.note(DepKey::Class(id));
        let (id, info) = self.map.get_key_value(id)?;
        Some(ClassRef { id, info, deps: self.deps })
    }

    pub(crate) fn contains_key(&self, id: &ClassId) -> bool {
        self.note(DepKey::Class(id));
        self.map.contains_key(id)
    }
}

/// One class's entry. Its other fields read through `Deref`; the method
/// tables only through the methods here, which note the read.
#[derive(Clone, Copy)]
pub(crate) struct ClassRef<'a> {
    id: &'a ClassId,
    info: &'a ClassInfo,
    deps: Option<&'a DepRecorder>,
}

impl<'a> ClassRef<'a> {
    fn note(&self, key: DepKey<'_>) {
        if let Some(deps) = self.deps {
            deps.note(key);
        }
    }

    pub(crate) fn instance_method(&self, method: &Symbol) -> Option<&'a Ty> {
        self.note(DepKey::Instance(self.id, method));
        self.info.instance_methods.get(method)
    }

    pub(crate) fn class_method(&self, method: &Symbol) -> Option<&'a Ty> {
        self.note(DepKey::ClassMethod(self.id, method));
        self.info.class_methods.get(method)
    }

    pub(crate) fn has_instance_method(&self, method: &Symbol) -> bool {
        self.instance_method(method).is_some()
    }

    pub(crate) fn has_class_method(&self, method: &Symbol) -> bool {
        self.class_method(method).is_some()
    }

    /// The superclass, borrowed from the registry rather than from this
    /// handle, so an ancestor walk can hold it past the handle.
    pub(crate) fn parent_id(&self) -> Option<&'a ClassId> {
        self.info.parent.as_ref()
    }

    /// Every instance-method type, in table order.
    pub(crate) fn instance_method_types(&self) -> impl Iterator<Item = &'a Ty> + 'a {
        self.note(DepKey::AllInstance(self.id));
        self.info.instance_methods.values()
    }
}

impl std::ops::Deref for ClassRef<'_> {
    type Target = ClassInfo;

    fn deref(&self) -> &ClassInfo {
        self.info
    }
}
