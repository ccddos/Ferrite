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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfNavItem {
    pub label: String,
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
    pub page_jump_input: String,
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
            page_jump_input: String::new(),
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
            page_jump_input: String::new(),
        }
    }
}

pub fn parse_page_jump_input(input: &str, total_pages: usize) -> Option<usize> {
    let page = input.trim().parse::<usize>().ok()?;
    if page == 0 || page > total_pages {
        return None;
    }
    Some(page - 1)
}

pub fn outline_or_page_list(outline: &[PdfOutlineItem], total_pages: usize) -> Vec<PdfNavItem> {
    if outline.is_empty() {
        return (0..total_pages)
            .map(|page_index| PdfNavItem {
                label: format!("Page {}", page_index + 1),
                page_index,
                depth: 0,
            })
            .collect();
    }

    outline
        .iter()
        .map(|item| PdfNavItem {
            label: item.title.clone(),
            page_index: item.page_index,
            depth: item.depth,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_page_jump_input_accepts_one_based_page_numbers() {
        assert_eq!(parse_page_jump_input("12", 200), Some(11));
    }

    #[test]
    fn test_parse_page_jump_input_rejects_out_of_range_page_numbers() {
        assert_eq!(parse_page_jump_input("0", 200), None);
        assert_eq!(parse_page_jump_input("201", 200), None);
        assert_eq!(parse_page_jump_input("abc", 200), None);
    }

    #[test]
    fn test_outline_or_page_list_returns_page_list_when_outline_is_empty() {
        let items = outline_or_page_list(&[], 3);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].label, "Page 1");
        assert_eq!(items[2].page_index, 2);
    }
}
