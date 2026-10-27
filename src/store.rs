//! The unit store.
#[derive(Clone, Debug)]
pub struct Handle {
    pub offset: u64,
    pub len: u64,
}
