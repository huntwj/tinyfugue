//! Macro & trigger system.
//!
//! Corresponds to `macro.c` / `macro.h` in the C source.
//!
//! ## Terminology
//!
//! TF uses "macro" for any named, body-bearing rule — triggers, hooks, key
//! bindings, and `/def`-defined commands are all macros.  This module models
//! that unification.
//!
//! ## Dispatch model
//!
//! *Triggers* fire when incoming server text matches [`Macro::trig`].
//! *Hooks* fire when a named event (e.g. `CONNECT`, `SEND`) is raised.
//!
//! In both cases the C code walks a priority-sorted list and:
//!
//! 1. Executes every *fall-through* match immediately, in priority order.
//! 2. Among *non-fall-through* matches at the highest matching priority,
//!    picks exactly one at random and executes it.
//!
//! [`MacroStore::find_triggers`] and [`MacroStore::find_hooks`] implement
//! this logic and return [`TriggerAction`] values for the caller to execute.

use std::collections::HashMap;

use crate::attr::Attr;
use crate::hook::{Hook, HookSet};
use crate::pattern::Pattern;

// ── Macro ─────────────────────────────────────────────────────────────────────

/// A TF macro: a rule that can fire on server text, hook events, and/or key
/// input and execute a script body in response.
///
/// Corresponds to `struct Macro` in `macro.c`.
#[derive(Debug, Clone)]
pub struct Macro {
    /// Unique sequential ID assigned at definition time.
    pub num: u32,
    /// Optional name (`/def name …`).  Unnamed macros are legal.
    pub name: Option<String>,
    /// Script body executed when the macro fires.
    pub body: Option<String>,
    /// Guard expression — body only runs when this evaluates truthy (`-E`).
    pub expr: Option<String>,
    /// Raw key sequence this macro is bound to (`-b`).
    pub bind: Option<String>,
    /// Human-readable key name such as `"F1"` (`-B`).
    pub keyname: Option<String>,
    /// Trigger pattern matched against incoming server text (`-t`).
    pub trig: Option<Pattern>,
    /// Hook argument pattern; matched against the hook's argument string (`-h`).
    pub hargs: Option<Pattern>,
    /// World-type pattern (e.g. `"tiny"`, `"lp"`) (`-T`).
    pub wtype: Option<Pattern>,
    /// Set of hooks this macro listens to (`-h CONNECT|DISCONNECT|…`).
    pub hooks: HookSet,
    /// Restrict to text from this named world (`-w`); `None` means any world.
    pub world: Option<String>,
    /// Matching priority — higher fires first. Default: 1.
    pub priority: i32,
    /// Probability 0–100 that the body executes on a match (`-P`). Default: 100.
    pub probability: u8,
    /// Remaining one-shot count; 0 = unlimited (`-n`).
    pub shots: u32,
    /// Text attribute applied when the macro matches (highlight / gag) (`-a`).
    pub attr: Attr,
    /// When `true`, lower-priority macros may also fire on the same text (`-f`).
    pub fallthru: bool,
    /// Suppress the "triggered N macros" feedback line (`-q`).
    pub quiet: bool,
    /// Hide from `/listdefs` output (`-i`).
    pub invisible: bool,
}

impl Macro {
    /// Construct an unnamed macro with sensible defaults.
    ///
    /// The `num` field is filled in by [`MacroStore::add`]; pass `0` here.
    pub fn new() -> Self {
        Self {
            num: 0,
            name: None,
            body: None,
            expr: None,
            bind: None,
            keyname: None,
            trig: None,
            hargs: None,
            wtype: None,
            hooks: HookSet::NONE,
            world: None,
            priority: 1,
            probability: 100,
            shots: 0,
            attr: Attr::EMPTY,
            fallthru: false,
            quiet: false,
            invisible: false,
        }
    }

    /// `true` if this macro participates in trigger matching.
    pub fn is_trigger(&self) -> bool {
        self.trig.is_some()
    }

    /// `true` if this macro responds to at least one hook event.
    pub fn is_hook(&self) -> bool {
        !self.hooks.is_empty()
    }
}

impl Default for Macro {
    fn default() -> Self {
        Self::new()
    }
}

impl Macro {
    /// Serialize to a `/def` command string suitable for reloading with `/load`.
    pub fn to_def_command(&self) -> String {
        use crate::pattern::MatchMode;
        use crate::hook::Hook;

        let mut flags = String::new();

        // Name
        if let Some(name) = &self.name {
            flags.push_str(&format!(" {name}"));
        }

        // Invisible
        if self.invisible { flags.push_str(" -i"); }

        // Priority (only emit when non-default)
        if self.priority != 1 {
            flags.push_str(&format!(" -p{}", self.priority));
        }

        // Shots
        if self.shots > 0 {
            flags.push_str(&format!(" -n{}", self.shots));
        }

        // Probability
        if self.probability != 100 {
            flags.push_str(&format!(" -P{}", self.probability));
        }

        // Fall-through
        if self.fallthru { flags.push_str(" -f"); }

        // Quiet
        if self.quiet { flags.push_str(" -q"); }

        // Attributes / gag
        if !self.attr.is_empty() {
            flags.push_str(&format!(" -a{}", self.attr.to_tf_flag()));
        }

        // Key binding
        if let Some(key) = &self.bind {
            flags.push_str(&format!(" -b'{key}'"));
        } else if let Some(kn) = &self.keyname {
            flags.push_str(&format!(" -B'{kn}'"));
        }

        // World scope
        if let Some(w) = &self.world {
            flags.push_str(&format!(" -w{w}"));
        }

        // World type
        if let Some(p) = &self.wtype {
            flags.push_str(&format!(" -T'{}'", p.src()));
        }

        // Hooks
        if !self.hooks.is_empty() {
            let names: Vec<&str> = Hook::ALL.iter()
                .filter(|&&h| self.hooks.contains(h))
                .map(|h| h.name())
                .collect();
            let hspec = if let Some(p) = &self.hargs {
                format!(" -h'{} {}'", names.join("|"), p.src())
            } else {
                format!(" -h'{}'", names.join("|"))
            };
            flags.push_str(&hspec);
        }

        // Guard expression
        if let Some(expr) = &self.expr {
            flags.push_str(&format!(" -E'{expr}'"));
        }

        // Trigger pattern
        if let Some(trig) = &self.trig {
            let mode_flag = match trig.mode() {
                MatchMode::Regexp => "-mregexp",
                MatchMode::Glob   => "-mglob",
                MatchMode::Simple => "-msimple",
                MatchMode::Substr => "-msubstr",
            };
            flags.push_str(&format!(" {mode_flag} -t'{}'", trig.src()));
        }

        let body = self.body.as_deref().unwrap_or("");
        format!("/def{flags} = {body}")
    }
}

// ── TriggerAction ─────────────────────────────────────────────────────────────

/// The outcome produced by a macro that fired against a line of text or a hook.
///
/// The caller (the event loop, once built) is responsible for:
/// * suppressing display when `gag` is `true`,
/// * merging `attr` into the line's display attributes,
/// * executing `body` through the script interpreter.
#[derive(Debug, Clone)]
pub struct TriggerAction {
    /// The line should be suppressed.
    pub gag: bool,
    /// Text attribute to merge into the display line.
    pub attr: Attr,
    /// Script body to execute (the macro's `/body`).
    pub body: Option<String>,
    /// Name or `#num` label (for diagnostics / mecho output).
    pub label: String,
}

// ── MacroStore ────────────────────────────────────────────────────────────────

/// Registry of all live macros; owns the trigger and hook dispatch lists.
///
/// Corresponds to the `maclist`, `triglist`, `hooklist[]`, and `macro_table`
/// globals in `macro.c`.
///
/// Invariants maintained by [`MacroStore::add`] and [`MacroStore::remove_by_num`]:
/// * `trig_list` is sorted by descending priority; at equal priority
///   fall-throughs appear before non-fall-throughs.
/// * Each `hook_lists[i]` obeys the same ordering.
#[derive(Debug)]
pub struct MacroStore {
    next_num: u32,
    /// All live macros keyed by `num`.
    macros: HashMap<u32, Macro>,
    /// Macro nums with triggers, in dispatch order (desc priority, fallthru first).
    trig_list: Vec<u32>,
    /// Per-hook lists (`Hook::COUNT` entries), same ordering as `trig_list`.
    hook_lists: Vec<Vec<u32>>,
    /// Name → num index for named macros.
    by_name: HashMap<String, u32>,
}

impl MacroStore {
    pub fn new() -> Self {
        Self {
            next_num: 1,
            macros: HashMap::new(),
            trig_list: Vec::new(),
            hook_lists: vec![Vec::new(); Hook::COUNT],
            by_name: HashMap::new(),
        }
    }

    /// Register a macro and return its assigned number.
    ///
    /// Inserts the macro into the trigger list and/or hook lists as
    /// appropriate, maintaining the invariant sort order.
    pub fn add(&mut self, mut mac: Macro) -> u32 {
        let num = self.next_num;
        self.next_num += 1;
        mac.num = num;

        if let Some(name) = &mac.name {
            self.by_name.insert(name.clone(), num);
        }

        if mac.is_trigger() {
            let pos = sorted_insert_pos(&self.trig_list, &self.macros, &mac);
            self.trig_list.insert(pos, num);
        }

        for &hook in Hook::ALL {
            if mac.hooks.contains(hook) {
                let idx = hook as usize;
                let pos = sorted_insert_pos(&self.hook_lists[idx], &self.macros, &mac);
                self.hook_lists[idx].insert(pos, num);
            }
        }

        self.macros.insert(num, mac);
        num
    }

    /// Remove a macro by number.  Returns `true` if it existed.
    pub fn remove_by_num(&mut self, num: u32) -> bool {
        let Some(mac) = self.macros.remove(&num) else {
            return false;
        };
        if let Some(name) = &mac.name {
            self.by_name.remove(name.as_str());
        }
        self.trig_list.retain(|&n| n != num);
        for list in &mut self.hook_lists {
            list.retain(|&n| n != num);
        }
        true
    }

    /// Remove macros matching `pattern` (compared against macro name).
    ///
    /// When `pattern` is `None`, removes all anonymous (unnamed) macros.
    /// Returns the count of macros removed.
    pub fn purge(&mut self, pattern: Option<&str>) -> usize {
        let to_remove: Vec<u32> = self
            .macros
            .iter()
            .filter(|(_, mac)| match pattern {
                None => mac.name.is_none(),
                Some(pat) => mac.name.as_deref().is_some_and(|n| n == pat || n.starts_with(pat)),
            })
            .map(|(&num, _)| num)
            .collect();
        let count = to_remove.len();
        for num in to_remove {
            self.remove_by_num(num);
        }
        count
    }

    /// Remove a macro by name.  Returns `true` if it existed.
    pub fn remove_by_name(&mut self, name: &str) -> bool {
        let Some(&num) = self.by_name.get(name) else {
            return false;
        };
        self.remove_by_num(num)
    }

    /// Look up a macro by number.
    pub fn get(&self, num: u32) -> Option<&Macro> {
        self.macros.get(&num)
    }

    /// Look up a macro by name.
    pub fn get_by_name(&self, name: &str) -> Option<&Macro> {
        self.by_name.get(name).and_then(|&n| self.macros.get(&n))
    }

    /// Number of macros currently stored.
    pub fn len(&self) -> usize {
        self.macros.len()
    }

    pub fn is_empty(&self) -> bool {
        self.macros.is_empty()
    }

    /// Iterate over all macros (unordered).
    pub fn iter(&self) -> impl Iterator<Item = &Macro> {
        self.macros.values()
    }

    // ── Trigger matching ──────────────────────────────────────────────────────

    /// Find all macros that trigger on `text` from `world`, return the
    /// [`TriggerAction`]s they produce in fire order.
    ///
    /// Mirrors the trigger path of `find_and_run_matches()` in `macro.c`:
    ///
    /// 1. Walk the trigger list in priority order (desc).
    /// 2. Fire each fall-through match immediately.
    /// 3. Collect non-fall-through matches at the highest matching priority,
    ///    then pick exactly one (random when `probability < 100`).
    pub fn find_triggers(&self, text: &str, world: Option<&str>) -> Vec<TriggerAction> {
        let mut actions: Vec<TriggerAction> = Vec::new();
        let mut nonfallthru: Vec<&Macro> = Vec::new();
        let mut lowest_nonfallthru_pri: Option<i32> = None;

        for &num in &self.trig_list {
            let mac = &self.macros[&num];

            // Once we've locked in the non-fallthru priority, skip lower ones.
            if let Some(limit) = lowest_nonfallthru_pri {
                if mac.priority < limit {
                    break;
                }
            }

            // World filter: if the macro names a specific world, text must come
            // from that world.
            if let Some(mac_world) = &mac.world {
                match world {
                    Some(w) if w.eq_ignore_ascii_case(mac_world) => {}
                    _ => continue,
                }
            }

            // Pattern match.
            let Some(pat) = &mac.trig else { continue };
            if !pat.matches(text) {
                continue;
            }

            if mac.fallthru {
                actions.push(action_from(mac));
            } else {
                lowest_nonfallthru_pri.get_or_insert(mac.priority);
                nonfallthru.push(mac);
            }
        }

        // Pick exactly one non-fall-through at the winning priority level,
        // weighted by probability.  (When all have prob=100 this is uniform.)
        if let Some(chosen) = pick_one(&nonfallthru) {
            actions.push(action_from(chosen));
        }

        actions
    }

    // ── Hook dispatch ─────────────────────────────────────────────────────────

    /// Find all macros registered for `hook` whose `hargs` pattern (if any)
    /// matches `args`.  Returns their [`TriggerAction`]s in fire order.
    ///
    /// Mirrors the hook path of `find_and_run_matches()` in `macro.c`.
    pub fn find_hooks(&self, hook: Hook, args: &str) -> Vec<TriggerAction> {
        let idx = hook as usize;
        let mut actions: Vec<TriggerAction> = Vec::new();
        let mut nonfallthru: Vec<&Macro> = Vec::new();
        let mut lowest_nonfallthru_pri: Option<i32> = None;

        for &num in &self.hook_lists[idx] {
            let mac = &self.macros[&num];

            if let Some(limit) = lowest_nonfallthru_pri {
                if mac.priority < limit {
                    break;
                }
            }

            // If the macro specifies an hargs pattern it must match.
            if let Some(pat) = &mac.hargs {
                if !pat.matches(args) {
                    continue;
                }
            }

            if mac.fallthru {
                actions.push(action_from(mac));
            } else {
                lowest_nonfallthru_pri.get_or_insert(mac.priority);
                nonfallthru.push(mac);
            }
        }

        if let Some(chosen) = pick_one(&nonfallthru) {
            actions.push(action_from(chosen));
        }

        actions
    }
}

impl Default for MacroStore {
    fn default() -> Self {
        Self::new()
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Returns the insertion index that keeps `list` sorted: descending priority,
/// fall-throughs before non-fall-throughs at equal priority.
fn sorted_insert_pos(list: &[u32], macros: &HashMap<u32, Macro>, new: &Macro) -> usize {
    list.iter()
        .position(|&n| comes_before(new, &macros[&n]))
        .unwrap_or(list.len())
}

/// `true` when `a` should appear before `b` in dispatch order.
fn comes_before(a: &Macro, b: &Macro) -> bool {
    if a.priority != b.priority {
        a.priority > b.priority
    } else {
        // Same priority: fall-throughs first.
        a.fallthru && !b.fallthru
    }
}

/// Build a [`TriggerAction`] from a matching macro.
fn action_from(mac: &Macro) -> TriggerAction {
    TriggerAction {
        gag: mac.attr.contains(Attr::GAG),
        attr: mac.attr,
        body: mac.body.clone(),
        label: mac
            .name
            .clone()
            .unwrap_or_else(|| format!("#{}", mac.num)),
    }
}

/// Pick one macro from a list of non-fall-through candidates, respecting each
/// macro's [`Macro::probability`] field.
///
/// Algorithm: build a cumulative-weight array and pick a uniformly random
/// point within [0, total_weight).  If total weight is 0 (all macros have
/// `probability == 0`), returns `None`.
fn pick_one<'a>(candidates: &[&'a Macro]) -> Option<&'a Macro> {
    if candidates.is_empty() {
        return None;
    }
    if candidates.len() == 1 {
        let mac = candidates[0];
        // Still respect probability even for a single candidate.
        if mac.probability == 0 {
            return None;
        }
        if mac.probability < 100 {
            let roll = rand_u8() % 100;
            if roll >= mac.probability {
                return None;
            }
        }
        return Some(mac);
    }
    // Multiple candidates: weighted random selection.
    let total: u32 = candidates.iter().map(|m| m.probability as u32).sum();
    if total == 0 {
        return None;
    }
    let mut roll = rand_u64() % total as u64;
    for mac in candidates {
        let w = mac.probability as u64;
        if roll < w {
            return Some(mac);
        }
        roll -= w;
    }
    // Fallback (shouldn't be reached).
    candidates.last().copied()
}

// ── Minimal PRNG (xorshift64, thread-local) ───────────────────────────────────

fn os_rand_seed() -> u64 {
    use std::io::Read;
    let mut buf = [0u8; 8];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .is_err()
    {
        // Fallback: mix in current time if /dev/urandom is unavailable.
        use std::time::{SystemTime, UNIX_EPOCH};
        let ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(1);
        buf[..4].copy_from_slice(&ns.to_ne_bytes());
        buf[4..].copy_from_slice(&ns.wrapping_add(0x9e37_79b9).to_ne_bytes());
    }
    let seed = u64::from_ne_bytes(buf);
    // xorshift64 requires a non-zero seed.
    if seed == 0 { 0x517c_c1b7_2722_0a95 } else { seed }
}

fn rand_u64() -> u64 {
    use std::cell::Cell;
    thread_local! {
        static STATE: Cell<u64> = const { Cell::new(0) }; // initialised lazily on first call
    }
    STATE.with(|s| {
        let mut x = s.get();
        if x == 0 {
            x = os_rand_seed();
        }
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        s.set(x);
        x
    })
}

fn rand_u8() -> u8 {
    rand_u64() as u8
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pattern::MatchMode;

    fn trig_macro(pat: &str, priority: i32, fallthru: bool, body: &str) -> Macro {
        let mut m = Macro::new();
        m.trig = Some(Pattern::new(pat, MatchMode::Substr).unwrap());
        m.priority = priority;
        m.fallthru = fallthru;
        m.body = Some(body.to_owned());
        m
    }

    fn hook_macro(hook: Hook, body: &str) -> Macro {
        let mut m = Macro::new();
        m.hooks = HookSet::from(hook);
        m.body = Some(body.to_owned());
        m
    }

    // ── add / remove ──────────────────────────────────────────────────────────

    #[test]
    fn add_and_get_by_name() {
        let mut store = MacroStore::new();
        let mut m = Macro::new();
        m.name = Some("greet".to_owned());
        m.body = Some("/echo hi".to_owned());
        let num = store.add(m);
        assert!(store.get(num).is_some());
        assert!(store.get_by_name("greet").is_some());
    }

    #[test]
    fn remove_by_num() {
        let mut store = MacroStore::new();
        let num = store.add(trig_macro("hello", 1, false, "/echo hi"));
        assert_eq!(store.len(), 1);
        assert!(store.remove_by_num(num));
        assert!(store.is_empty());
        assert!(store.get(num).is_none());
    }

    #[test]
    fn remove_by_name() {
        let mut store = MacroStore::new();
        let mut m = Macro::new();
        m.name = Some("byebye".to_owned());
        store.add(m);
        assert!(store.remove_by_name("byebye"));
        assert!(store.get_by_name("byebye").is_none());
    }

    #[test]
    fn remove_missing_returns_false() {
        let mut store = MacroStore::new();
        assert!(!store.remove_by_num(999));
        assert!(!store.remove_by_name("nobody"));
    }

    // ── Priority ordering ──────────────────────────────────────────────────────

    #[test]
    fn trig_list_sorted_descending() {
        let mut store = MacroStore::new();
        // Add in non-priority order.
        store.add(trig_macro("a", 1, false, "low"));
        store.add(trig_macro("b", 5, false, "high"));
        store.add(trig_macro("c", 3, false, "mid"));

        // The internal trig_list should be [5, 3, 1].
        let priorities: Vec<i32> = store
            .trig_list
            .iter()
            .map(|&n| store.macros[&n].priority)
            .collect();
        assert_eq!(priorities, vec![5, 3, 1]);
    }

    #[test]
    fn fallthru_before_nonfallthru_at_same_priority() {
        let mut store = MacroStore::new();
        store.add(trig_macro("a", 5, false, "non-ft"));
        store.add(trig_macro("b", 5, true, "ft"));

        // fall-through should come first.
        let first = store.macros[&store.trig_list[0]].fallthru;
        assert!(first, "fall-through should precede non-fall-through");
    }

    // ── find_triggers ─────────────────────────────────────────────────────────

    #[test]
    fn no_match_returns_empty() {
        let mut store = MacroStore::new();
        store.add(trig_macro("goblin", 1, false, "/echo goblin"));
        let actions = store.find_triggers("a dragon appears", None);
        assert!(actions.is_empty());
    }

    #[test]
    fn single_trigger_fires() {
        let mut store = MacroStore::new();
        store.add(trig_macro("dragon", 1, false, "/echo DRAGON!"));
        let actions = store.find_triggers("a dragon appears", None);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].body.as_deref(), Some("/echo DRAGON!"));
    }

    #[test]
    fn fallthru_fires_and_lets_others_through() {
        let mut store = MacroStore::new();
        // Both match "dragon"; ft fires first.
        store.add(trig_macro("dragon", 5, true, "/echo FT"));
        store.add(trig_macro("dragon", 5, false, "/echo NOFT"));

        let actions = store.find_triggers("a dragon roars", None);
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0].body.as_deref(), Some("/echo FT"));
        assert_eq!(actions[1].body.as_deref(), Some("/echo NOFT"));
    }

    #[test]
    fn only_highest_priority_nonfallthru_wins() {
        let mut store = MacroStore::new();
        // High-priority wins; low-priority should NOT appear.
        store.add(trig_macro("dragon", 10, false, "/echo HIGH"));
        store.add(trig_macro("dragon", 1, false, "/echo LOW"));

        let actions = store.find_triggers("a dragon", None);
        // Exactly one non-fallthru fires, and it must be the high-priority one.
        let nonfts: Vec<_> = actions.iter().filter(|a| !a.gag).collect();
        assert_eq!(nonfts.len(), 1);
        assert_eq!(nonfts[0].body.as_deref(), Some("/echo HIGH"));
    }

    #[test]
    fn world_filter_respected() {
        let mut store = MacroStore::new();
        let mut m = trig_macro("orc", 1, false, "/echo ORC");
        m.world = Some("Avalon".to_owned());
        store.add(m);

        // Fires for Avalon.
        assert_eq!(store.find_triggers("an orc attacks", Some("Avalon")).len(), 1);
        // Does NOT fire for a different world.
        assert!(store.find_triggers("an orc attacks", Some("Pax")).is_empty());
        // Does NOT fire with no world.
        assert!(store.find_triggers("an orc attacks", None).is_empty());
    }

    #[test]
    fn gag_attr_propagates() {
        let mut store = MacroStore::new();
        let mut m = trig_macro("spam", 1, false, "");
        m.attr = Attr::GAG;
        store.add(m);

        let actions = store.find_triggers("spam spam spam", None);
        assert_eq!(actions.len(), 1);
        assert!(actions[0].gag);
    }

    // ── find_hooks ────────────────────────────────────────────────────────────

    #[test]
    fn hook_fires_on_matching_event() {
        let mut store = MacroStore::new();
        store.add(hook_macro(Hook::Connect, "/echo connected!"));
        let actions = store.find_hooks(Hook::Connect, "Avalon");
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].body.as_deref(), Some("/echo connected!"));
    }

    #[test]
    fn hook_does_not_fire_on_wrong_event() {
        let mut store = MacroStore::new();
        store.add(hook_macro(Hook::Connect, "/echo connected!"));
        assert!(store.find_hooks(Hook::Disconnect, "Avalon").is_empty());
    }

    #[test]
    fn hook_hargs_pattern_filters() {
        let mut store = MacroStore::new();
        let mut m = hook_macro(Hook::Connect, "/echo avalon only");
        m.hargs = Some(Pattern::new("Avalon", MatchMode::Substr).unwrap());
        store.add(m);

        // Matches when args contain "Avalon".
        assert_eq!(
            store.find_hooks(Hook::Connect, "Avalon 23").len(),
            1
        );
        // Does NOT match for a different world name.
        assert!(store.find_hooks(Hook::Connect, "Pax 23").is_empty());
    }

    #[test]
    fn hook_fallthru_and_nonfallthru_both_fire() {
        let mut store = MacroStore::new();
        let mut ft = hook_macro(Hook::Send, "/echo FT");
        ft.fallthru = true;
        store.add(ft);
        store.add(hook_macro(Hook::Send, "/echo NOFT"));

        let actions = store.find_hooks(Hook::Send, "anything");
        assert_eq!(actions.len(), 2);
    }

    #[test]
    fn zero_probability_macro_never_fires() {
        let mut store = MacroStore::new();
        let mut m = trig_macro("dragon", 1, false, "/echo nope");
        m.probability = 0;
        store.add(m);

        // Run many times; should never fire.
        for _ in 0..50 {
            let actions = store.find_triggers("a dragon appears", None);
            assert!(actions.is_empty(), "prob=0 macro fired unexpectedly");
        }
    }
}
