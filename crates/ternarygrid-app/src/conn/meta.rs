//! Lazily loaded, cached metadata per connection (navigator, autocomplete, ERD).

use crate::backend::{Backend, Job, Loadable};
use crate::conn::session::SessionHandle;
use std::collections::HashMap;
use std::sync::Arc;
use ternarygrid_core::DbResult;
use ternarygrid_core::catalog::{CatalogContext, CatalogFolder, CatalogRow};
use ternarygrid_core::ddl::{SchemaSnapshot, SnapshotOptions, TableDef};
use ternarygrid_core::meta::{DatabaseInfo, ObjectInfo, ObjectKind, ObjectRef, SchemaInfo, SchemaRef};

/// A listing of the navigator: the objects of a schema's folder, or the children
/// of an object (a table's partitions).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ListKey {
    Objects(SchemaRef, ObjectKind),
    Children(ObjectRef, ObjectKind),
}

impl ListKey {
    /// The database whose session reads the listing.
    pub fn database(&self) -> Option<&str> {
        match self {
            ListKey::Objects(s, _) => s.database.as_deref(),
            ListKey::Children(o, _) => o.database.as_deref(),
        }
    }

    fn schema(&self) -> SchemaRef {
        match self {
            ListKey::Objects(s, _) => s.clone(),
            ListKey::Children(o, _) => o.schema_ref(),
        }
    }

    fn parent(&self) -> Option<&ObjectRef> {
        match self {
            ListKey::Objects(..) => None,
            ListKey::Children(o, _) => Some(o),
        }
    }
}

/// The listing shows sizes or row counts (tables, partitions), which change with the rows.
fn shows_sizes(list: &[ObjectInfo]) -> bool {
    list.iter().any(|o| o.size_bytes.is_some() || o.row_estimate.is_some())
}

#[derive(Default)]
pub struct MetadataCache {
    pub databases: Loadable<Vec<DatabaseInfo>>,
    /// Keyed by database ("" = the session's current database).
    pub schemas: HashMap<String, Loadable<Vec<SchemaInfo>>>,
    pub objects: HashMap<(SchemaRef, ObjectKind), Loadable<Vec<ObjectInfo>>>,
    pub children: HashMap<(ObjectRef, ObjectKind), Loadable<Vec<ObjectInfo>>>,
    /// Light snapshots (tables, views, columns, keys) for autocomplete and ER diagrams.
    pub snapshots: HashMap<SchemaRef, Loadable<Arc<SchemaSnapshot>>>,
    pub table_defs: HashMap<ObjectRef, Loadable<Arc<TableDef>>>,
    /// Catalog folders (roles, settings, users …), keyed by folder id and context.
    pub catalog: HashMap<(&'static str, CatalogContext), Loadable<Vec<CatalogRow>>>,
    /// Incremented whenever any entry finishes loading; consumers rebuild derived data.
    pub generation: u64,
    /// Counts the clears (refresh, DDL): open object editors reload after one.
    pub invalidations: u64,
    /// Listings reading their sizes again after the rows changed (TRUNCATE, import):
    /// the loaded list stays on screen until the new one replaces it.
    reloads: HashMap<ListKey, Job<DbResult<Vec<ObjectInfo>>>>,
}

fn db_key(db: Option<&str>) -> String {
    db.unwrap_or("").to_string()
}

impl MetadataCache {
    /// Move finished loads into place. Returns true if anything changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = self.databases.poll();
        for v in self.schemas.values_mut() {
            changed |= v.poll();
        }
        for v in self.objects.values_mut() {
            changed |= v.poll();
        }
        for v in self.children.values_mut() {
            changed |= v.poll();
        }
        for v in self.snapshots.values_mut() {
            changed |= v.poll();
        }
        for v in self.table_defs.values_mut() {
            changed |= v.poll();
        }
        for v in self.catalog.values_mut() {
            changed |= v.poll();
        }
        changed |= self.poll_reloads();
        if changed {
            self.generation += 1;
        }
        changed
    }

    pub fn is_loading(&self) -> bool {
        self.databases.is_loading()
            || self.schemas.values().any(Loadable::is_loading)
            || self.objects.values().any(Loadable::is_loading)
            || self.children.values().any(Loadable::is_loading)
            || self.snapshots.values().any(Loadable::is_loading)
            || self.catalog.values().any(Loadable::is_loading)
    }

    /// Forget everything (refresh).
    pub fn clear(&mut self) {
        *self = MetadataCache { generation: self.generation + 1, invalidations: self.invalidations + 1, ..Default::default() };
    }

    /// Forget the loads that failed (their session was lost meanwhile): they load
    /// again when shown.
    pub fn forget_failed(&mut self) {
        if self.databases.error().is_some() {
            self.databases = Loadable::NotLoaded;
        }
        self.schemas.retain(|_, l| l.error().is_none());
        self.objects.retain(|_, l| l.error().is_none());
        self.children.retain(|_, l| l.error().is_none());
        self.snapshots.retain(|_, l| l.error().is_none());
        self.table_defs.retain(|_, l| l.error().is_none());
        self.catalog.retain(|_, l| l.error().is_none());
        self.generation += 1;
    }

    /// Forget one schema's objects and snapshot.
    pub fn clear_schema(&mut self, schema: &SchemaRef) {
        self.objects.retain(|(s, _), _| s != schema);
        self.children.retain(|(o, _), _| o.schema_ref() != *schema);
        self.snapshots.remove(schema);
        self.table_defs.retain(|o, _| o.schema_ref() != *schema);
        self.reloads.retain(|k, _| k.schema() != *schema);
        self.generation += 1;
        self.invalidations += 1;
    }

    /// Forget a whole database: its schema list, objects and snapshots (a script's
    /// DDL may change any schema, or create a database). `current` is the
    /// connection's database, which references name either way (`None` or by name).
    pub fn clear_database(&mut self, database: Option<&str>, current: Option<&str>) {
        let norm = |d: Option<&str>| d.filter(|d| !d.is_empty() && Some(*d) != current).map(str::to_string);
        let db = norm(database);
        let same = |s: &SchemaRef| norm(s.database.as_deref()) == db;
        self.objects.retain(|(s, _), _| !same(s));
        self.children.retain(|(o, _), _| !same(&o.schema_ref()));
        self.snapshots.retain(|s, _| !same(s));
        self.table_defs.retain(|o, _| !same(&o.schema_ref()));
        self.reloads.retain(|k, _| !same(&k.schema()));
        self.schemas.retain(|k, _| norm(Some(k)) != db);
        if !self.databases.is_loading() {
            self.databases = Loadable::NotLoaded;
        }
        self.generation += 1;
        self.invalidations += 1;
    }

    /// The loaded listings of `scope` showing sizes or row counts: those of one schema,
    /// or of a whole database when `scope` names none (`current`: as in
    /// [`clear_database`](Self::clear_database)).
    pub fn size_lists(&self, scope: &SchemaRef, current: Option<&str>) -> Vec<ListKey> {
        let norm = |d: Option<&str>| d.filter(|d| !d.is_empty() && Some(*d) != current).map(str::to_string);
        let in_scope = |s: &SchemaRef| match scope.schema {
            Some(_) => s == scope,
            None => norm(s.database.as_deref()) == norm(scope.database.as_deref()),
        };
        let mut keys = Vec::new();
        // A partition may sit in another schema than its table, whose size is the sum of its
        // partitions': that table's listing reads its sizes again too.
        let mut parents: Vec<SchemaRef> = Vec::new();
        for ((parent, kind), l) in &self.children {
            let Some(list) = l.get().filter(|v| shows_sizes(v)) else { continue };
            if in_scope(&parent.schema_ref()) || list.iter().any(|c| in_scope(&c.obj.schema_ref())) {
                keys.push(ListKey::Children(parent.clone(), *kind));
                parents.push(parent.schema_ref());
            }
        }
        for ((schema, kind), l) in &self.objects {
            if l.get().is_some_and(|v| shows_sizes(v)) && (in_scope(schema) || parents.contains(schema)) {
                keys.push(ListKey::Objects(schema.clone(), *kind));
            }
        }
        keys
    }

    /// Read a loaded listing again in the background, keeping it shown meanwhile
    /// (no "Loading…" in its place). A listing not loaded is left to load when shown.
    pub fn reload_list(&mut self, backend: &Backend, session: &SessionHandle, key: ListKey) {
        if !self.list(&key).is_some_and(|l| l.get().is_some()) {
            return;
        }
        let s = session.clone();
        let job = match key.clone() {
            ListKey::Objects(schema, kind) => backend.spawn(async move { s.lock().await.list_objects(&schema, kind).await }),
            ListKey::Children(parent, kind) => backend.spawn(async move { s.lock().await.list_children(&parent, kind).await }),
        };
        // A newer reload replaces one still running (its result would be older).
        self.reloads.insert(key, job);
    }

    /// Forget a listing (it loads again when shown): the rows changed where no session is open yet.
    pub fn forget_list(&mut self, key: &ListKey) {
        match key {
            ListKey::Objects(s, k) => self.objects.remove(&(s.clone(), *k)),
            ListKey::Children(o, k) => self.children.remove(&(o.clone(), *k)),
        };
        self.reloads.remove(key);
        self.generation += 1;
    }

    fn list(&self, key: &ListKey) -> Option<&Loadable<Vec<ObjectInfo>>> {
        match key {
            ListKey::Objects(s, k) => self.objects.get(&(s.clone(), *k)),
            ListKey::Children(o, k) => self.children.get(&(o.clone(), *k)),
        }
    }

    fn list_mut(&mut self, key: &ListKey) -> Option<&mut Loadable<Vec<ObjectInfo>>> {
        match key {
            ListKey::Objects(s, k) => self.objects.get_mut(&(s.clone(), *k)),
            ListKey::Children(o, k) => self.children.get_mut(&(o.clone(), *k)),
        }
    }

    /// Put the finished reloads in place. A listing cleared or loading again meanwhile
    /// (Refresh, DDL) is not brought back; a failed reload keeps the list shown.
    fn poll_reloads(&mut self) -> bool {
        let mut done = Vec::new();
        self.reloads.retain(|key, job| match job.poll() {
            Some(r) => {
                done.push((key.clone(), r));
                false
            }
            None => true,
        });
        let mut changed = false;
        for (key, r) in done {
            match (self.list_mut(&key), r) {
                (Some(l @ Loadable::Loaded(_)), Ok(v)) => {
                    *l = Loadable::Loaded(v);
                    changed = true;
                }
                (_, Err(e)) => tracing::debug!("reloading the sizes of {key:?}: {}", e.full_text()),
                _ => {}
            }
        }
        changed
    }

    pub fn clear_object(&mut self, obj: &ObjectRef) {
        self.children.retain(|(o, _), _| o != obj);
        self.reloads.retain(|k, _| k.parent() != Some(obj));
        self.table_defs.remove(obj);
        self.snapshots.remove(&obj.schema_ref());
        self.generation += 1;
        self.invalidations += 1;
    }

    pub fn request_databases(&mut self, backend: &Backend, session: &SessionHandle) {
        if self.databases.is_not_loaded() {
            let s = session.clone();
            self.databases = Loadable::Loading(backend.spawn(async move { s.lock().await.list_databases().await }));
        }
    }

    pub fn schemas_of(&mut self, database: Option<&str>) -> &mut Loadable<Vec<SchemaInfo>> {
        self.schemas.entry(db_key(database)).or_default()
    }

    /// The schemas of `database`, once read.
    pub fn schema_list(&self, database: Option<&str>) -> Option<&[SchemaInfo]> {
        self.schemas.get(&db_key(database)).and_then(|l| l.get()).map(Vec::as_slice)
    }

    pub fn request_schemas(&mut self, backend: &Backend, session: &SessionHandle, database: Option<&str>) {
        let entry = self.schemas.entry(db_key(database)).or_default();
        if entry.is_not_loaded() {
            let s = session.clone();
            *entry = Loadable::Loading(backend.spawn(async move { s.lock().await.list_schemas().await }));
        }
    }

    pub fn request_objects(&mut self, backend: &Backend, session: &SessionHandle, schema: &SchemaRef, kind: ObjectKind) {
        let entry = self.objects.entry((schema.clone(), kind)).or_default();
        if entry.is_not_loaded() {
            let s = session.clone();
            let schema = schema.clone();
            *entry = Loadable::Loading(backend.spawn(async move { s.lock().await.list_objects(&schema, kind).await }));
        }
    }

    pub fn request_children(&mut self, backend: &Backend, session: &SessionHandle, parent: &ObjectRef, kind: ObjectKind) {
        let entry = self.children.entry((parent.clone(), kind)).or_default();
        if entry.is_not_loaded() {
            let s = session.clone();
            let parent = parent.clone();
            *entry = Loadable::Loading(backend.spawn(async move { s.lock().await.list_children(&parent, kind).await }));
        }
    }

    pub fn request_catalog(&mut self, backend: &Backend, session: &SessionHandle, folder: &CatalogFolder, ctx: &CatalogContext) {
        let entry = self.catalog.entry((folder.id, ctx.clone())).or_default();
        if entry.is_not_loaded() {
            let s = session.clone();
            let (folder, ctx) = (folder.clone(), ctx.clone());
            *entry = Loadable::Loading(backend.spawn(async move { ternarygrid_core::catalog::list(s.lock().await.as_mut(), &folder, &ctx).await }));
        }
    }

    /// Forget one catalog folder (refresh).
    pub fn clear_catalog(&mut self, folder: &str) {
        self.catalog.retain(|(f, _), _| *f != folder);
        self.generation += 1;
    }

    pub fn request_snapshot(&mut self, backend: &Backend, session: &SessionHandle, schema: &SchemaRef) {
        let entry = self.snapshots.entry(schema.clone()).or_default();
        if entry.is_not_loaded() {
            let s = session.clone();
            let schema = schema.clone();
            *entry = Loadable::Loading(backend.spawn(async move { s.lock().await.schema_snapshot(&schema, &SnapshotOptions::light()).await.map(Arc::new) }));
        }
    }

    pub fn request_table_def(&mut self, backend: &Backend, session: &SessionHandle, table: &ObjectRef) {
        let entry = self.table_defs.entry(table.clone()).or_default();
        if entry.is_not_loaded() {
            let s = session.clone();
            let t = table.clone();
            *entry = Loadable::Loading(backend.spawn(load_relation_definition(s, t)));
        }
    }

    pub fn snapshot(&self, schema: &SchemaRef) -> Option<Arc<SchemaSnapshot>> {
        self.snapshots.get(schema).and_then(|l| l.get()).cloned()
    }

    pub fn table_def(&self, table: &ObjectRef) -> Option<Arc<TableDef>> {
        self.table_defs.get(table).and_then(|l| l.get()).cloned()
    }

    /// All snapshots loaded so far.
    pub fn loaded_snapshots(&self) -> Vec<Arc<SchemaSnapshot>> {
        self.snapshots.values().filter_map(|l| l.get().cloned()).collect()
    }
}

/// Loads a table or view definition from a session.
///
/// Most SQL drivers (PostgreSQL, SQLite, Oracle, MSSQL, etc.) reject views in
/// `table_definition` and serve them via `view_definition`; falling back to
/// `table_definition` keeps drivers that handle both uniformly working too.
pub(crate) async fn load_relation_definition(session: SessionHandle, obj: ObjectRef) -> DbResult<Arc<TableDef>> {
    let mut s = session.lock().await;
    if matches!(obj.kind, ObjectKind::View | ObjectKind::MaterializedView) {
        return match s.view_definition(&obj).await {
            Ok(v) => {
                let mut t = TableDef::new(v.schema, v.name);
                t.columns = v.columns;
                t.comment = v.comment;
                t.options = v.options;
                Ok(Arc::new(t))
            }
            Err(view_err) => match s.table_definition(&obj).await {
                Ok(t) => Ok(Arc::new(t)),
                Err(_) => Err(view_err),
            },
        };
    }
    s.table_definition(&obj).await.map(Arc::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(db: Option<&str>, name: &str) -> SchemaRef {
        SchemaRef { database: db.map(str::to_string), schema: Some(name.to_string()) }
    }

    fn table(s: &SchemaRef, name: &str, size: Option<i64>) -> ObjectInfo {
        let mut obj = ObjectRef::table(s.schema.clone(), name);
        obj.database = s.database.clone();
        let mut o = ObjectInfo::new(obj);
        o.size_bytes = size;
        o
    }

    fn wait<T>(job: &Job<T>) {
        let t0 = std::time::Instant::now();
        while !job.is_finished() {
            assert!(t0.elapsed() < std::time::Duration::from_secs(5), "job never finished");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    fn sorted(mut keys: Vec<ListKey>) -> Vec<String> {
        let mut v: Vec<String> = keys.drain(..).map(|k| format!("{k:?}")).collect();
        v.sort();
        v
    }

    /// The listings showing sizes in the truncated table's schema reload; views, other
    /// schemas and listings not loaded do not.
    #[test]
    fn size_lists_of_a_schema() {
        let (public, other) = (schema(Some("app"), "public"), schema(Some("app"), "other"));
        let mut m = MetadataCache::default();
        m.objects.insert((public.clone(), ObjectKind::Table), Loadable::Loaded(vec![table(&public, "charges", Some(6 << 30))]));
        m.objects.insert((public.clone(), ObjectKind::View), Loadable::Loaded(vec![ObjectInfo::new(ObjectRef::new(ObjectKind::View, Some("public".into()), "v"))]));
        m.objects.insert((public.clone(), ObjectKind::MaterializedView), Loadable::NotLoaded);
        m.objects.insert((other.clone(), ObjectKind::Table), Loadable::Loaded(vec![table(&other, "t", Some(8192))]));
        let parent = table(&public, "charges", None).obj;
        let mut part = ObjectInfo::new(ObjectRef::child(ObjectKind::Partition, &parent, "charges_2026_02"));
        part.size_bytes = Some(5 << 30);
        m.children.insert((parent.clone(), ObjectKind::Partition), Loadable::Loaded(vec![part]));
        m.children.insert((parent.clone(), ObjectKind::Column), Loadable::Loaded(vec![ObjectInfo::new(ObjectRef::child(ObjectKind::Column, &parent, "id"))]));

        let keys = m.size_lists(&public, Some("app"));
        assert_eq!(sorted(keys), sorted(vec![ListKey::Objects(public.clone(), ObjectKind::Table), ListKey::Children(parent.clone(), ObjectKind::Partition)]));
        // The whole database: both schemas (named or as the current database).
        assert_eq!(m.size_lists(&SchemaRef { database: Some("app".into()), schema: None }, Some("app")).len(), 3);
        assert_eq!(m.size_lists(&SchemaRef { database: None, schema: None }, Some("app")).len(), 3);
        assert!(m.size_lists(&SchemaRef { database: Some("billing".into()), schema: None }, Some("app")).is_empty());
    }

    /// A partition in another schema than its table: the table's listing (its size sums the
    /// partitions') reloads with the partitions'.
    #[test]
    fn size_lists_follow_a_partition_to_its_table() {
        let (public, archive) = (schema(None, "public"), schema(None, "archive"));
        let mut m = MetadataCache::default();
        m.objects.insert((public.clone(), ObjectKind::Table), Loadable::Loaded(vec![table(&public, "charges", Some(12 << 30))]));
        let parent = table(&public, "charges", None).obj;
        let mut part = ObjectInfo::new(ObjectRef::child(ObjectKind::Partition, &parent, "charges_2025"));
        part.obj.schema = Some("archive".into());
        part.size_bytes = Some(6 << 30);
        m.children.insert((parent.clone(), ObjectKind::Partition), Loadable::Loaded(vec![part]));
        let keys = m.size_lists(&archive, None);
        assert_eq!(sorted(keys), sorted(vec![ListKey::Objects(public.clone(), ObjectKind::Table), ListKey::Children(parent, ObjectKind::Partition)]));
    }

    /// A reload keeps the old list shown, then replaces it; a listing cleared meanwhile
    /// (Refresh) is not brought back, and a failed reload keeps the list.
    #[test]
    fn reloads_replace_the_list_in_place() {
        let backend = Backend::new().unwrap();
        let public = schema(None, "public");
        let key = ListKey::Objects(public.clone(), ObjectKind::Table);
        let mut m = MetadataCache::default();
        m.objects.insert((public.clone(), ObjectKind::Table), Loadable::Loaded(vec![table(&public, "charges", Some(6 << 30))]));

        let fresh = vec![table(&public, "charges", Some(8192))];
        let job = backend.spawn(async move { Ok(fresh) });
        wait(&job);
        // Until it is in place, the old list shows (no "Loading…").
        assert_eq!(m.objects[&(public.clone(), ObjectKind::Table)].get().unwrap()[0].size_bytes, Some(6 << 30));
        m.reloads.insert(key.clone(), job);
        let generation = m.generation;
        assert!(m.poll());
        assert!(m.generation > generation);
        assert_eq!(m.objects[&(public.clone(), ObjectKind::Table)].get().unwrap()[0].size_bytes, Some(8192));
        assert!(m.reloads.is_empty());

        // Failed: the list stays.
        m.reloads.insert(key.clone(), backend.spawn(async { Err(ternarygrid_core::DbError::query("gone")) }));
        let t0 = std::time::Instant::now();
        while !m.reloads.is_empty() {
            m.poll();
            assert!(t0.elapsed() < std::time::Duration::from_secs(5));
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(m.objects[&(public.clone(), ObjectKind::Table)].get().unwrap()[0].size_bytes, Some(8192));

        // Cleared meanwhile: the pending reload goes with it.
        let stale = vec![table(&public, "charges", Some(1))];
        m.reloads.insert(key.clone(), backend.spawn(async move { Ok(stale) }));
        m.clear_schema(&public);
        assert!(m.reloads.is_empty());
        assert!(!m.objects.contains_key(&(public.clone(), ObjectKind::Table)));

        // Forgotten where no session is at hand: it loads again when shown.
        m.objects.insert((public.clone(), ObjectKind::Table), Loadable::Loaded(vec![table(&public, "charges", Some(1))]));
        m.forget_list(&key);
        assert!(!m.objects.contains_key(&(public, ObjectKind::Table)));
    }
}
