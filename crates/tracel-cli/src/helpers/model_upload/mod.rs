//! Uploading a model version: making sure the model exists, and uploading its parts in
//! parallel.

mod existence;
mod parts;
mod upload;

pub use existence::ensure_model_exists;
pub use parts::build_part_tasks;
pub use upload::upload_parts;
