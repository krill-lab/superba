//! Custom widgets in the Geist style. Every widget reads the active palette from
//! the context ([`Palette::of`]), animates over [`ANIM_SECS`], draws a focus ring when
//! it has keyboard focus and reports AccessKit info.

mod badge;
mod button;
mod card;
mod countdown;
mod dialog;
mod key_input;
mod latency;
mod meter;
mod segmented;
mod status;
mod table;
mod text;
mod toast;
mod toggle;

// v0.3 widgets (UX §4). Public modules, so their builders can add items without
// editing this file (it is not edited during Phase 1).
pub mod choice_card;
pub mod line_tile;
pub mod paged_table;
pub mod pager;
pub mod pending_bar;
pub mod speed_tiers;
// Long setting values in a fixed width (user bug 15).
pub mod value_chip;
// Owned by B6.
pub mod token_field;
// Owned by B7.
pub mod detail_pane;
pub mod info_card;
pub mod log_table;
pub mod quote_preview;
pub mod source_list;
pub mod toolbar;
// The one dropdown every page uses (fixed height, border inside its rectangle).
pub mod dropdown;

pub use badge::Badge;
pub use button::{Button, ButtonKind, ButtonSize, IconButton};
pub use card::{Card, card};
pub use countdown::CountdownBar;
pub use dialog::{
    WINDOW_MARGIN as DIALOG_WINDOW_MARGIN, bounded, dialog, dialog_body,
    max_content_height as dialog_max_content_height,
};
pub use key_input::KeyInput;
pub use latency::{Latency, LatencyBadge};
pub use meter::{Meter, SplitBar};
pub use segmented::{SegmentLayout, Segmented};
pub use status::{Status, StatusDot};
pub use table::Table;
pub use text::{Text, kbd, link_button};
pub use toast::{Toast, Toasts};
pub use toggle::Toggle;

pub use choice_card::{Choice, ChoiceCard, choice_cards};
pub use detail_pane::DetailPane;
pub use dropdown::Dropdown;
pub use info_card::{FactCard, InfoCard};
pub use line_tile::{LineTile, tile_columns};
pub use log_table::LogTable;
pub use paged_table::{PagedResponse, PagedState, PagedTable, rows_per_page};
pub use pager::Pager;
pub use pending_bar::{PendingAction, PendingBar, PendingLabels};
pub use quote_preview::QuotePreview;
pub use source_list::SourceList;
pub use speed_tiers::{SpeedTiers, TierText};
pub use token_field::TokenField;
pub use toolbar::Toolbar;

// Label + controls rows on one center line (every form row of every page).
pub mod form_row;
pub use form_row::{FormRow, FormRowResponse};

// Keyboard focus rings: when they show (keyboard only) and where (never clipped).
pub mod focus;
pub use focus::{focus_ring, focus_ring_inside, focus_visible, show_focus};

use crate::design::tokens::ANIM_SECS;

/// Hover animation factor for a response (0 = idle, 1 = hovered).
pub fn hover_t(ui: &egui::Ui, response: &egui::Response) -> f32 {
    ui.ctx().animate_bool_with_time(
        response.id.with("hover"),
        response.hovered() && ui.is_enabled(),
        ANIM_SECS,
    )
}

// Dashboard pieces after Surge (user request 24): KPI figures, charts, pills and
// list rows.
pub mod chart;
pub mod kpi;
pub mod list_row;
pub mod pill;
pub use kpi::{Kpi, KpiValue};
pub use list_row::{ListRow, RowContent, ShareRow, paint_row};
pub use pill::{NoticePill, StatusPill};

pub mod progress;
pub mod tile;
pub use progress::InlineProgress;
pub use tile::{IconTile, MonogramTile};

pub mod sub_nav;
pub use sub_nav::{SubNav, SubNavItem};

pub mod underline_tabs;
pub use underline_tabs::{UnderlineTab, UnderlineTabs};

pub mod attention;
pub use attention::AttentionRow;
// Show / hide a group (「高级」, details): a chevron header with its state.
pub mod disclosure;
pub use disclosure::{Disclosure, DisclosureResponse};
pub mod update_row;
pub use update_row::{UpdateGroup, UpdateResponse, UpdateRow, UpdateTile};

pub mod search_field;
pub use search_field::SearchField;
