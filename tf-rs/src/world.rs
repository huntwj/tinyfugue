//! World connection profiles.
//!
//! Corresponds to `world.c` / `world.h` in the C source.

// ── WorldFlags ────────────────────────────────────────────────────────────────

/// Connection flags for a [`World`].
///
/// Maps to the `WORLD_*` bit constants in `world.h`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorldFlags {
    /// Use SSL/TLS (`-x` / `WORLD_SSL`).
    pub ssl: bool,
    /// Do not route through the proxy (`-p` / `WORLD_NOPROXY`).
    pub no_proxy: bool,
    /// Echo sent lines back to the user (`-e` / `WORLD_ECHO`).
    pub echo: bool,
    /// Unnamed/temporary world created at connect-time (`WORLD_TEMP`).
    pub temp: bool,
}

// ── World ─────────────────────────────────────────────────────────────────────

/// A named MUD server connection profile.
///
/// Corresponds to `struct World` in `world.h`.  Runtime-only fields
/// (socket, screen, history, trigger/hook lists) belong to later phases.
#[derive(Debug, Clone)]
pub struct World {
    pub name: String,
    /// User-defined server type, e.g. `"tiny"`, `"lp"`, `"diku"`.
    pub world_type: Option<String>,
    pub host: Option<String>,
    pub port: Option<String>,
    pub character: Option<String>,
    pub pass: Option<String>,
    /// Macro file to source on connect (`%{mfile}`).
    pub mfile: Option<String>,
    /// Preferred local source address (`-s` / C: `myhost`).
    pub myhost: Option<String>,
    pub flags: WorldFlags,
}

impl World {
    /// Create a world with only a name set; all other fields are `None`.
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            world_type: None,
            host: None,
            port: None,
            character: None,
            pass: None,
            mfile: None,
            myhost: None,
            flags: WorldFlags::default(),
        }
    }

    /// Returns `true` if both `host` and `port` are set.
    pub fn is_connectable(&self) -> bool {
        self.host.is_some() && self.port.is_some()
    }

    /// Serialize this world as a `/addworld` command line.
    ///
    /// Format mirrors the C TF output:
    /// `/addworld [-Ttype] [-s myhost] [-e] [-x] [-p] name[=char[,pass]] host port [mfile]`
    pub fn to_addworld(&self) -> String {
        let mut parts: Vec<String> = vec!["/addworld".to_owned()];

        if let Some(t) = &self.world_type {
            parts.push(format!("-T{t}"));
        }
        if let Some(h) = &self.myhost {
            parts.push("-s".to_owned());
            parts.push(h.clone());
        }
        if self.flags.echo    { parts.push("-e".to_owned()); }
        if self.flags.ssl     { parts.push("-x".to_owned()); }
        if self.flags.no_proxy { parts.push("-p".to_owned()); }

        // name[=char[,pass]]
        let mut name_field = self.name.clone();
        if let Some(ch) = &self.character {
            name_field.push('=');
            name_field.push_str(ch);
            if let Some(pw) = &self.pass {
                name_field.push(',');
                name_field.push_str(pw);
            }
        }
        parts.push(name_field);

        if let Some(host) = &self.host {
            parts.push(host.clone());
        }
        if let Some(port) = &self.port {
            parts.push(port.clone());
        }
        if let Some(mf) = &self.mfile {
            parts.push(mf.clone());
        }

        parts.join(" ")
    }
}

// ── WorldStore ────────────────────────────────────────────────────────────────

/// Registry of all known worlds.
///
/// Mirrors the `hworld` linked list and `defaultworld` globals in `world.c`.
/// Named worlds are stored in insertion order.
#[derive(Debug, Default)]
pub struct WorldStore {
    worlds: Vec<World>,
    default_world: Option<World>,
}

impl WorldStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a world by name.
    ///
    /// The name `"default"` (case-insensitive) is stored in the separate
    /// `defaultworld` slot.  Returns `true` if a *new* world was added,
    /// `false` if an existing one was updated.
    pub fn upsert(&mut self, world: World) -> bool {
        if world.name.eq_ignore_ascii_case("default") {
            self.default_world = Some(world);
            return false;
        }
        if let Some(slot) = self.worlds.iter_mut()
            .find(|w| w.name.eq_ignore_ascii_case(&world.name))
        {
            *slot = world;
            false
        } else {
            self.worlds.push(world);
            true
        }
    }

    /// Find a world by name (case-insensitive).
    ///
    /// `"default"` returns the default world.  Returns `None` if not found.
    pub fn find(&self, name: &str) -> Option<&World> {
        if name.eq_ignore_ascii_case("default") {
            return self.default_world.as_ref();
        }
        self.worlds.iter().find(|w| w.name.eq_ignore_ascii_case(name))
    }

    /// Remove a world by name (case-insensitive).  Returns `true` if it existed.
    pub fn remove(&mut self, name: &str) -> bool {
        if name.eq_ignore_ascii_case("default") {
            return self.default_world.take().is_some();
        }
        let before = self.worlds.len();
        self.worlds.retain(|w| !w.name.eq_ignore_ascii_case(name));
        self.worlds.len() < before
    }

    /// The "default" world, if one has been configured.
    pub fn default_world(&self) -> Option<&World> {
        self.default_world.as_ref()
    }

    /// Iterate over all named worlds in insertion order.
    ///
    /// Does **not** include the default world; access that via
    /// [`WorldStore::default_world`].
    pub fn iter(&self) -> impl Iterator<Item = &World> {
        self.worlds.iter()
    }

    pub fn len(&self) -> usize {
        self.worlds.len()
    }

    pub fn is_empty(&self) -> bool {
        self.worlds.is_empty()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_and_find() {
        let mut store = WorldStore::new();
        let mut w = World::named("Avalon");
        w.host = Some("avalon.mud.net".into());
        w.port = Some("23".into());
        assert!(store.upsert(w));
        let found = store.find("Avalon").unwrap();
        assert_eq!(found.host.as_deref(), Some("avalon.mud.net"));
    }

    #[test]
    fn find_is_case_insensitive() {
        let mut store = WorldStore::new();
        store.upsert(World::named("Pax"));
        assert!(store.find("PAX").is_some());
        assert!(store.find("pax").is_some());
    }

    #[test]
    fn upsert_replaces_existing() {
        let mut store = WorldStore::new();
        let mut w1 = World::named("test");
        w1.port = Some("1234".into());
        assert!(store.upsert(w1));

        let mut w2 = World::named("test");
        w2.port = Some("5678".into());
        assert!(!store.upsert(w2)); // false = updated, not new

        assert_eq!(store.find("test").unwrap().port.as_deref(), Some("5678"));
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn default_world() {
        let mut store = WorldStore::new();
        let mut d = World::named("default");
        d.character = Some("mychar".into());
        assert!(!store.upsert(d)); // default always returns false (update)
        assert!(store.is_empty()); // default is not in the named list
        assert_eq!(store.default_world().unwrap().character.as_deref(), Some("mychar"));
    }

    #[test]
    fn remove_world() {
        let mut store = WorldStore::new();
        store.upsert(World::named("Keeper"));
        store.upsert(World::named("Gone"));
        assert!(store.remove("Gone"));
        assert!(store.find("Gone").is_none());
        assert!(store.find("Keeper").is_some());
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn remove_missing_returns_false() {
        let mut store = WorldStore::new();
        assert!(!store.remove("nobody"));
    }

    #[test]
    fn is_connectable() {
        let mut w = World::named("test");
        assert!(!w.is_connectable());
        w.host = Some("host".into());
        assert!(!w.is_connectable()); // port still missing
        w.port = Some("23".into());
        assert!(w.is_connectable());
    }

    #[test]
    fn to_addworld_minimal() {
        let mut w = World::named("mud");
        w.host = Some("mud.example.com".into());
        w.port = Some("4000".into());
        assert_eq!(w.to_addworld(), "/addworld mud mud.example.com 4000");
    }

    #[test]
    fn to_addworld_full() {
        let mut w = World::named("mud");
        w.world_type = Some("lp".into());
        w.myhost = Some("192.168.1.1".into());
        w.flags.ssl = true;
        w.flags.no_proxy = true;
        w.character = Some("player".into());
        w.pass = Some("secret".into());
        w.host = Some("mud.example.com".into());
        w.port = Some("4000".into());
        w.mfile = Some("~/mud.tf".into());
        assert_eq!(
            w.to_addworld(),
            "/addworld -Tlp -s 192.168.1.1 -x -p mud=player,secret mud.example.com 4000 ~/mud.tf"
        );
    }

    #[test]
    fn iter_preserves_insertion_order() {
        let mut store = WorldStore::new();
        for name in ["Alpha", "Beta", "Gamma"] {
            store.upsert(World::named(name));
        }
        let names: Vec<_> = store.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["Alpha", "Beta", "Gamma"]);
    }
}
