//! A method body, on the heap or in the unit store ([`crate::store`]).
//!
//! `Body` dereferences to its `Expr` wherever it is. A stored body is
//! decoded from the store on the first read and kept until it is spilled
//! again; a mutable borrow brings it back to the heap for good. Every
//! reader therefore sees the same tree whether or not it was stored.

use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::expr::Expr;
use crate::store::{Store, Stored};

#[derive(Clone, Debug)]
pub struct Body(State);

#[derive(Clone, Debug)]
enum State {
    Resident(Expr),
    Stored(Stored),
}

impl Body {
    pub fn new(expr: Expr) -> Self {
        Body(State::Resident(expr))
    }

    /// The tree, owned.
    pub fn into_expr(self) -> Expr {
        match self.0 {
            State::Resident(expr) => expr,
            State::Stored(stored) => stored.take(),
        }
    }

    /// Keep this body in `store` and drop it from the heap. A body that
    /// is already stored, and was only read since, is not written again:
    /// its decoded copy is dropped. A body with no stored form stays.
    pub fn spill(&mut self, store: &Arc<Store>) {
        if let State::Stored(stored) = &mut self.0
            && Arc::ptr_eq(stored.store(), store)
        {
            stored.evict();
            return;
        }
        if let Some(handle) = store.write(&**self) {
            self.0 = State::Stored(Stored::new(Arc::clone(store), handle));
        }
    }

    /// Run `read` over the tree. A stored body is decoded for the call
    /// and not kept, so a walk over many stored bodies holds one at a time.
    pub fn with_expr<R>(&self, read: impl FnOnce(&Expr) -> R) -> R {
        match &self.0 {
            State::Resident(expr) => read(expr),
            State::Stored(stored) => stored.with(read),
        }
    }

    /// The tree when it is on the heap; `None` when it is in the store,
    /// without decoding it.
    pub fn resident(&self) -> Option<&Expr> {
        match &self.0 {
            State::Resident(expr) => Some(expr),
            State::Stored(_) => None,
        }
    }
}

impl From<Expr> for Body {
    fn from(expr: Expr) -> Self {
        Body::new(expr)
    }
}

impl std::ops::Deref for Body {
    type Target = Expr;
    fn deref(&self) -> &Expr {
        match &self.0 {
            State::Resident(expr) => expr,
            State::Stored(stored) => stored.get(),
        }
    }
}

impl std::ops::DerefMut for Body {
    fn deref_mut(&mut self) -> &mut Expr {
        if matches!(self.0, State::Stored(_)) {
            let placeholder = State::Resident(Expr::new(
                crate::span::Span::synthetic(),
                crate::expr::ExprNode::Lit { value: crate::expr::Literal::Nil },
            ));
            if let State::Stored(stored) = std::mem::replace(&mut self.0, placeholder) {
                self.0 = State::Resident(stored.take());
            }
        }
        match &mut self.0 {
            State::Resident(expr) => expr,
            State::Stored(_) => unreachable!("a body is resident after it is taken out of the store"),
        }
    }
}

impl PartialEq for Body {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

impl Serialize for Body {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        (**self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Body {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Expr::deserialize(deserializer).map(Body::new)
    }
}
