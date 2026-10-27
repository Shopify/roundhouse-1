//! A method body that the unit store can move to disk.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::expr::Expr;

#[derive(Clone, Debug)]
pub struct Body(State);

#[derive(Clone, Debug)]
enum State {
    Resident(Expr),
    Spilled(crate::store::Handle),
}

impl Body {
    pub fn new(expr: Expr) -> Self {
        Body(State::Resident(expr))
    }

    /// The tree, which must be resident.
    pub fn into_expr(self) -> Expr {
        match self.0 {
            State::Resident(expr) => expr,
            State::Spilled(handle) => panic!("method body taken while spilled to the unit store ({handle:?})"),
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
            State::Spilled(handle) => panic!("method body read while spilled to the unit store ({handle:?})"),
        }
    }
}

impl std::ops::DerefMut for Body {
    fn deref_mut(&mut self) -> &mut Expr {
        match &mut self.0 {
            State::Resident(expr) => expr,
            State::Spilled(handle) => panic!("method body written while spilled to the unit store ({handle:?})"),
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
