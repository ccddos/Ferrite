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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdfMouseMode {
    Pan,
    Cursor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PdfOutlineItem {
    pub title: String,
    pub page_index: usize,
    pub depth: usize,
}

/// Per-tab UI state for PDF preview.
#[derive(Debug, Clone)]
pub struct PdfPreviewState {
    pub current_page: usize,
    pub zoom: f32,
    pub total_pages: Option<usize>,
    pub spread_mode: PdfSpreadMode,
    pub scroll_layout: PdfScrollLayout,
    pub sidebar_visible: bool,
    pub sidebar_mode: PdfSidebarMode,
    pub mouse_mode: PdfMouseMode,
}

impl Default for PdfPreviewState {
    fn default() -> Self {
        Self {
            current_page: 0,
            zoom: 1.0,
            total_pages: None,
            spread_mode: PdfSpreadMode::TwoPageOdd,
            scroll_layout: PdfScrollLayout::Vertical,
            sidebar_visible: true,
            sidebar_mode: PdfSidebarMode::Thumbnails,
            mouse_mode: PdfMouseMode::Pan,
        }
    }
}

impl PdfPreviewState {
    pub fn to_snapshot(&self) -> PdfViewStateSnapshot {
        PdfViewStateSnapshot {
            current_page: self.current_page,
            zoom_percent: (self.zoom * 100.0).round() as u16,
            spread_mode: self.spread_mode,
            scroll_layout: self.scroll_layout,
            sidebar_visible: self.sidebar_visible,
            sidebar_mode: self.sidebar_mode,
        }
    }

    pub fn from_snapshot(snapshot: PdfViewStateSnapshot) -> Self {
        Self {
            current_page: snapshot.current_page,
            zoom: (snapshot.zoom_percent as f32 / 100.0).clamp(0.5, 4.0),
            total_pages: None,
            spread_mode: snapshot.spread_mode,
            scroll_layout: snapshot.scroll_layout,
            sidebar_visible: snapshot.sidebar_visible,
            sidebar_mode: snapshot.sidebar_mode,
            mouse_mode: PdfMouseMode::Pan,
        }
    }
}
