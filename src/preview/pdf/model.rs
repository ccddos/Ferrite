use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PdfSpreadMode {
    SinglePage,
    TwoPageOdd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PdfScrollLayout {
    Vertical,
    Horizontal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PdfSidebarMode {
    Thumbnails,
    Outline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PdfViewStateSnapshot {
    pub current_page: usize,
    pub zoom_percent: u16,
    pub spread_mode: PdfSpreadMode,
    pub scroll_layout: PdfScrollLayout,
    pub sidebar_visible: bool,
    pub sidebar_mode: PdfSidebarMode,
}

impl Default for PdfViewStateSnapshot {
    fn default() -> Self {
        Self {
            current_page: 0,
            zoom_percent: 100,
            spread_mode: PdfSpreadMode::TwoPageOdd,
            scroll_layout: PdfScrollLayout::Vertical,
            sidebar_visible: true,
            sidebar_mode: PdfSidebarMode::Thumbnails,
        }
    }
}
