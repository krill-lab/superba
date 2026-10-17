//! String newtype identifiers.
//!
//! Ids are stable, lowercase, ASCII and persisted in `profiles.json` / `state.json`,
//! so never derive them from display names that may be translated.

use std::borrow::Borrow;
use std::fmt;

use serde::{Deserialize, Serialize};

macro_rules! string_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            /// Wrap a string.
            pub fn new(id: impl Into<String>) -> Self {
                Self(id.into())
            }

            /// Borrow the raw id.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl Borrow<str> for $name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }

        impl PartialEq<str> for $name {
            fn eq(&self, other: &str) -> bool {
                self.0 == other
            }
        }

        impl PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool {
                self.0 == *other
            }
        }
    };
}

string_id!(
    /// Identifies a [`crate::Site`] (`krill`, `official`, or a user-created compatible site).
    SiteId
);
string_id!(
    /// Identifies a [`crate::Line`] within its site (`cn-fast`, `overseas`, `cdn`, ...).
    LineId
);
string_id!(
    /// Identifies a [`crate::Profile`]. Built-in profiles use `original` and `official`.
    ProfileId
);
string_id!(
    /// Identifies a [`crate::Target`]: `local` for this machine (app and CLI share
    /// one `CODEX_HOME`), `ssh:<key>` for a remote host.
    TargetId
);

impl TargetId {
    /// The id of the local machine target.
    pub const LOCAL: &'static str = "local";

    /// `local`.
    pub fn local() -> Self {
        Self(Self::LOCAL.to_owned())
    }

    /// `ssh:<key>` where key is the app `hostId` when known, else the ssh alias or host.
    pub fn remote(key: &str) -> Self {
        Self(format!("ssh:{key}"))
    }

    /// True for the local target.
    pub fn is_local(&self) -> bool {
        self.0 == Self::LOCAL
    }
}
