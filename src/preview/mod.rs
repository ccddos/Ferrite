//! Preview and sync scrolling module for Ferrite
//!
//! This module provides synchronized scrolling between Raw and Rendered
//! markdown views, allowing users to see corresponding content in both panes.

mod image_preview;
mod sync_scroll;

// Note: ScrollOrigin is available for future split-view bidirectional sync scrolling
#[allow(unused_imports)]
pub use image_preview::render_image_preview;
#[allow(unused_imports)]
pub use sync_scroll::{ScrollOrigin, SyncScrollState};
