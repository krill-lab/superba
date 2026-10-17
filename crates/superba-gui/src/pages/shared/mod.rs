//! Helpers shared by several pages, so no page imports another page's module
//! (plan §4 C2): automatic-auth decisions and line order (Overview, Providers,
//! wizard, router), profile creation, number formatting and the provider list
//! (Providers, Models, Settings) and setting values for display (Settings, the
//! switch preview, Remote hosts).

pub mod auth;
pub mod format;
pub mod hosts;
pub mod lines;
pub mod profile;
pub mod providers;
pub mod values;

pub use auth::{AutoFacts, auto_facts, decision, decision_with, switch_action};
pub use format::{compact, group_thousands, nice_axis};
pub use lines::{moved, ordered_lines};
pub use profile::new_profile;
pub use providers::{Provider, ProviderId, ProviderKind};
