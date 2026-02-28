//! Hook event types.
//!
//! Corresponds to `hooklist.h` in the C source.  The C code uses an X-macro
//! pattern to generate both an enum and a table; here we use a plain Rust enum
//! with an explicit discriminant so that `hook as usize` gives a stable index
//! into `MacroStore`'s per-hook dispatch lists.

use std::str::FromStr;

// ── Hook ──────────────────────────────────────────────────────────────────────

/// A hook event that TF can fire.
///
/// Variants are in the same alphabetical order as `hooklist.h` so that their
/// discriminant values are stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(usize)]
pub enum Hook {
    Activity    =  0,
    Bamf        =  1,
    BgText      =  2,
    BgTrig      =  3,
    ConFail     =  4,
    Conflict    =  5,
    Connect     =  6,
    Disconnect  =  7,
    IConFail    =  8,
    Kill        =  9,
    Load        = 10,
    LoadFail    = 11,
    Log         = 12,
    Login       = 13,
    Mail        = 14,
    More        = 15,
    NoMacro     = 16,
    Pending     = 17,
    PreActivity = 18,
    Process     = 19,
    Prompt      = 20,
    Proxy       = 21,
    Redef       = 22,
    Resize      = 23,
    Send        = 24,
    Shadow      = 25,
    Shell       = 26,
    SigHup      = 27,
    SigTerm     = 28,
    SigUsr1     = 29,
    SigUsr2     = 30,
    World       = 31,
    // Protocol-extension hooks (conditional in C TF via ENABLE_ATCP / ENABLE_GMCP /
    // ENABLE_OPTION102).
    Atcp        = 32,
    Gmcp        = 33,
    Option102   = 34,
}

impl Hook {
    /// Every hook variant in the same order as the C `hook_table[]`.
    pub const ALL: &'static [Hook] = &[
        Hook::Activity,
        Hook::Bamf,
        Hook::BgText,
        Hook::BgTrig,
        Hook::ConFail,
        Hook::Conflict,
        Hook::Connect,
        Hook::Disconnect,
        Hook::IConFail,
        Hook::Kill,
        Hook::Load,
        Hook::LoadFail,
        Hook::Log,
        Hook::Login,
        Hook::Mail,
        Hook::More,
        Hook::NoMacro,
        Hook::Pending,
        Hook::PreActivity,
        Hook::Process,
        Hook::Prompt,
        Hook::Proxy,
        Hook::Redef,
        Hook::Resize,
        Hook::Send,
        Hook::Shadow,
        Hook::Shell,
        Hook::SigHup,
        Hook::SigTerm,
        Hook::SigUsr1,
        Hook::SigUsr2,
        Hook::World,
        Hook::Atcp,
        Hook::Gmcp,
        Hook::Option102,
    ];

    /// Total number of hook variants.
    pub const COUNT: usize = 35;

    /// The canonical uppercase name used in TF scripts (e.g. `"CONNECT"`).
    pub fn name(self) -> &'static str {
        match self {
            Hook::Activity    => "ACTIVITY",
            Hook::Bamf        => "BAMF",
            Hook::BgText      => "BGTEXT",
            Hook::BgTrig      => "BGTRIG",
            Hook::ConFail     => "CONFAIL",
            Hook::Conflict    => "CONFLICT",
            Hook::Connect     => "CONNECT",
            Hook::Disconnect  => "DISCONNECT",
            Hook::IConFail    => "ICONFAIL",
            Hook::Kill        => "KILL",
            Hook::Load        => "LOAD",
            Hook::LoadFail    => "LOADFAIL",
            Hook::Log         => "LOG",
            Hook::Login       => "LOGIN",
            Hook::Mail        => "MAIL",
            Hook::More        => "MORE",
            Hook::NoMacro     => "NOMACRO",
            Hook::Pending     => "PENDING",
            Hook::PreActivity => "PREACTIVITY",
            Hook::Process     => "PROCESS",
            Hook::Prompt      => "PROMPT",
            Hook::Proxy       => "PROXY",
            Hook::Redef       => "REDEF",
            Hook::Resize      => "RESIZE",
            Hook::Send        => "SEND",
            Hook::Shadow      => "SHADOW",
            Hook::Shell       => "SHELL",
            Hook::SigHup      => "SIGHUP",
            Hook::SigTerm     => "SIGTERM",
            Hook::SigUsr1     => "SIGUSR1",
            Hook::SigUsr2     => "SIGUSR2",
            Hook::World       => "WORLD",
            Hook::Atcp        => "ATCP",
            Hook::Gmcp        => "GMCP",
            Hook::Option102   => "OPTION102",
        }
    }
}

impl FromStr for Hook {
    type Err = String;

    /// Case-insensitive parse. Accepts `"BACKGROUND"` as an alias for
    /// `BgTrig` (backward compatibility with old TF scripts).
    fn from_str(s: &str) -> Result<Self, String> {
        let upper = s.to_ascii_uppercase();
        if upper == "BACKGROUND" {
            return Ok(Hook::BgTrig);
        }
        Hook::ALL
            .iter()
            .copied()
            .find(|h| h.name() == upper)
            .ok_or_else(|| format!("invalid hook event {:?}", s))
    }
}

// ── HookSet ───────────────────────────────────────────────────────────────────

/// A set of zero or more [`Hook`] variants, stored as a 64-bit bitmask.
///
/// Corresponds to `hookvec_t` (a bit vector) in the C source.  There are 32
/// hook variants so a `u64` gives plenty of headroom.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HookSet(u64);

impl HookSet {
    /// The empty set.
    pub const NONE: Self = Self(0);
    /// The universal set (every hook).
    pub const ALL: Self = Self(u64::MAX);

    /// Returns `true` if `hook` is in this set.
    #[inline]
    pub fn contains(self, hook: Hook) -> bool {
        self.0 & (1u64 << hook as u64) != 0
    }

    /// Add `hook` to the set.
    #[inline]
    pub fn insert(&mut self, hook: Hook) {
        self.0 |= 1u64 << hook as u64;
    }

    /// Returns `true` if no hooks are in the set.
    #[inline]
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl From<Hook> for HookSet {
    fn from(h: Hook) -> Self {
        let mut s = HookSet::NONE;
        s.insert(h);
        s
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_matches_all_len() {
        assert_eq!(Hook::COUNT, Hook::ALL.len());
    }

    #[test]
    fn round_trip_from_str() {
        for &h in Hook::ALL {
            let parsed: Hook = h.name().parse().unwrap();
            assert_eq!(parsed, h);
        }
    }

    #[test]
    fn case_insensitive_parse() {
        let h: Hook = "connect".parse().unwrap();
        assert_eq!(h, Hook::Connect);
    }

    #[test]
    fn background_alias() {
        let h: Hook = "BACKGROUND".parse().unwrap();
        assert_eq!(h, Hook::BgTrig);
    }

    #[test]
    fn unknown_hook_errors() {
        assert!("XYZZY".parse::<Hook>().is_err());
    }

    #[test]
    fn hookset_insert_contains() {
        let mut s = HookSet::NONE;
        assert!(!s.contains(Hook::Connect));
        s.insert(Hook::Connect);
        assert!(s.contains(Hook::Connect));
        assert!(!s.contains(Hook::Disconnect));
    }

    #[test]
    fn hookset_all_contains_every_hook() {
        for &h in Hook::ALL {
            assert!(HookSet::ALL.contains(h));
        }
    }

    #[test]
    fn hookset_from_hook() {
        let s = HookSet::from(Hook::Mail);
        assert!(s.contains(Hook::Mail));
        assert!(!s.contains(Hook::Kill));
    }

    #[test]
    fn discriminants_are_dense() {
        // Each variant's discriminant must equal its index in ALL.
        for (i, &h) in Hook::ALL.iter().enumerate() {
            assert_eq!(h as usize, i, "{:?} has wrong discriminant", h);
        }
    }
}
