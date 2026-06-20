pub(crate) mod catalog;
pub(crate) mod codec;
pub(crate) mod table;

pub(crate) type RowId = u64;

pub(crate) use catalog::Catalog;
pub(crate) use table::Table;
