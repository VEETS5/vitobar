// Real system tray via StatusNotifierItem (SNI) D-Bus protocol.
//
// Implements StatusNotifierWatcher, tracks registered tray items,
// loads their icons, and fetches their DBusMenu for right-click context menus.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// ── Public types shared with the main bar ────────────────────────────────────

#[derive(Debug, Clone)]
pub struct TrayItem {
    pub id:         String,
    pub title:      String,
    pub icon_name:  String,
    pub icon_rgba:  Option<Vec<u8>>, // pre-decoded RGBA pixels if pixmap provided
    pub icon_size:  u32,             // side length when icon_rgba is Some
    pub menu_path:  String,          // D-Bus object path for com.canonical.dbusmenu
    pub service:    String,          // D-Bus bus name
}

#[derive(Debug, Clone)]
pub struct MenuItem {
    pub id:      i32,
    pub label:   String,
    pub enabled: bool,
    pub is_separator: bool,
}

pub type TrayState = Arc<Mutex<Vec<TrayItem>>>;

// ── Background thread entry point ────────────────────────────────────────────

/// Spawns a background thread that runs the SNI watcher and keeps `state`
/// up to date. Call once at startup. The returned `TrayState` is polled by
/// the bar's drawing code.
pub fn spawn_tray_watcher() -> TrayState {
    let state: TrayState = Arc::new(Mutex::new(Vec::new()));
    let state2 = Arc::clone(&state);

    std::thread::spawn(move || {
        // Build a single-threaded tokio runtime for zbus async
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime for tray");

        rt.block_on(async move {
            if let Err(e) = run_watcher(state2).await {
                log::error!("tray watcher failed: {e}");
            }
        });
    });

    state
}

// ── SNI Watcher implementation ───────────────────────────────────────────────

use zbus::{Connection, interface, message::Header, proxy};

struct WatcherState {
    items: Vec<String>,           // registered service names / object paths
    host_registered: bool,
}

struct StatusNotifierWatcher {
    inner: Arc<Mutex<WatcherState>>,
}

#[interface(name = "org.kde.StatusNotifierWatcher")]
impl StatusNotifierWatcher {
    async fn register_status_notifier_item(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        service_or_path: &str,
    ) {
        let sender = hdr.sender().map(|s| s.as_str().to_string()).unwrap_or_default();
        let full_name = if service_or_path.starts_with('/') {
            // It's an object path — combine with sender bus name
            format!("{}{}", sender, service_or_path)
        } else {
            service_or_path.to_string()
        };
        log::info!("tray: item registered: {full_name}");
        let mut state = self.inner.lock().unwrap();
        if !state.items.contains(&full_name) {
            state.items.push(full_name.clone());
        }
    }

    async fn register_status_notifier_host(&self, _service: &str) {
        self.inner.lock().unwrap().host_registered = true;
    }

    #[zbus(property)]
    async fn registered_status_notifier_items(&self) -> Vec<String> {
        self.inner.lock().unwrap().items.clone()
    }

    #[zbus(property)]
    async fn is_status_notifier_host_registered(&self) -> bool {
        self.inner.lock().unwrap().host_registered
    }

    #[zbus(property)]
    async fn protocol_version(&self) -> i32 {
        0
    }
}

// ── DBusMenu proxy for fetching menus ────────────────────────────────────────

#[proxy(
    interface = "com.canonical.dbusmenu",
    default_service = "org.kde.StatusNotifierItem",
    default_path = "/MenuBar"
)]
trait DbusmenuProxy {
    fn get_layout(
        &self,
        parent_id: i32,
        recursion_depth: i32,
        property_names: &[&str],
    ) -> zbus::Result<(u32, (i32, HashMap<String, zbus::zvariant::OwnedValue>, Vec<zbus::zvariant::OwnedValue>))>;

    fn event(
        &self,
        id: i32,
        event_id: &str,
        data: &zbus::zvariant::Value<'_>,
        timestamp: u32,
    ) -> zbus::Result<()>;
}

// ── StatusNotifierItem proxy for querying items ──────────────────────────────

#[proxy(
    interface = "org.kde.StatusNotifierItem",
    default_service = "org.kde.StatusNotifierItem",
    default_path = "/StatusNotifierItem"
)]
trait StatusNotifierItem {
    #[zbus(property)]
    fn id(&self) -> zbus::Result<String>;

    #[zbus(property)]
    fn title(&self) -> zbus::Result<String>;

    #[zbus(property)]
    fn icon_name(&self) -> zbus::Result<String>;

    #[zbus(property)]
    fn icon_pixmap(&self) -> zbus::Result<Vec<(i32, i32, Vec<u8>)>>;

    #[zbus(property)]
    fn menu(&self) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;

    fn activate(&self, x: i32, y: i32) -> zbus::Result<()>;

    fn secondary_activate(&self, x: i32, y: i32) -> zbus::Result<()>;

    fn context_menu(&self, x: i32, y: i32) -> zbus::Result<()>;
}

// ── Main watcher loop ────────────────────────────────────────────────────────

async fn run_watcher(tray_state: TrayState) -> zbus::Result<()> {
    let conn = Connection::session().await?;

    let watcher_inner = Arc::new(Mutex::new(WatcherState {
        items: Vec::new(),
        host_registered: true,
    }));

    let watcher = StatusNotifierWatcher {
        inner: Arc::clone(&watcher_inner),
    };

    // Serve the watcher interface
    conn.object_server()
        .at("/StatusNotifierWatcher", watcher)
        .await?;

    // Claim the well-known name
    conn.request_name("org.kde.StatusNotifierWatcher")
        .await?;

    log::info!("tray: StatusNotifierWatcher registered on D-Bus");

    // Poll for changes and refresh tray state
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        let item_names: Vec<String> = watcher_inner.lock().unwrap().items.clone();
        let mut new_items: Vec<TrayItem> = Vec::new();

        for raw in &item_names {
            // Parse "bus_name/object_path" or just "bus_name"
            let (bus_name, obj_path) = if let Some(idx) = raw.find('/') {
                // Check if it's like ":1.45/org/ayatana/..." pattern
                let bn = &raw[..idx];
                let op = &raw[idx..];
                (bn.to_string(), op.to_string())
            } else {
                (raw.clone(), "/StatusNotifierItem".to_string())
            };

            let proxy = match StatusNotifierItemProxy::builder(&conn)
                .destination(bus_name.as_str())
                .unwrap_or_else(|_| panic!("bad dest"))
                .path(obj_path.as_str())
                .unwrap_or_else(|_| panic!("bad path"))
                .build()
                .await
            {
                Ok(p) => p,
                Err(e) => {
                    log::debug!("tray: can't connect to {raw}: {e}");
                    continue;
                }
            };

            let id = proxy.id().await.unwrap_or_default();
            let title = proxy.title().await.unwrap_or_else(|_| id.clone());
            let icon_name = proxy.icon_name().await.unwrap_or_default();
            let menu_path = proxy.menu().await
                .map(|p| p.to_string())
                .unwrap_or_else(|_| "/MenuBar".to_string());

            // Try to get icon pixmap
            let (icon_rgba, icon_size) = match proxy.icon_pixmap().await {
                Ok(pixmaps) if !pixmaps.is_empty() => {
                    // Pick the largest pixmap
                    let best = pixmaps.iter().max_by_key(|(w, h, _)| w * h).unwrap();
                    let (w, _h, data) = best;
                    // SNI pixmaps are ARGB32 in network byte order, convert to RGBA
                    let rgba = argb_to_rgba(data);
                    (Some(rgba), *w as u32)
                }
                _ => (None, 0),
            };

            new_items.push(TrayItem {
                id,
                title,
                icon_name,
                icon_rgba,
                icon_size,
                menu_path,
                service: bus_name,
            });
        }

        // Prune items whose D-Bus service is gone
        // (items that failed proxy creation were already skipped)
        *tray_state.lock().unwrap() = new_items;
    }
}

/// Convert ARGB32 (network byte order) to RGBA8.
fn argb_to_rgba(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    for chunk in data.chunks(4) {
        if chunk.len() < 4 { break; }
        let a = chunk[0];
        let r = chunk[1];
        let g = chunk[2];
        let b = chunk[3];
        out.extend_from_slice(&[r, g, b, a]);
    }
    out
}

// ── Public helpers for the main bar ──────────────────────────────────────────

/// Fetch the DBusMenu for a tray item (blocking, call from a background thread).
pub fn fetch_menu_items(item: &TrayItem) -> Vec<MenuItem> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    let rt = match rt {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    rt.block_on(async {
        fetch_menu_items_async(item).await
    })
}

async fn fetch_menu_items_async(item: &TrayItem) -> Vec<MenuItem> {
    let conn = match Connection::session().await {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let proxy = match DbusmenuProxyProxy::builder(&conn)
        .destination(item.service.as_str())
        .unwrap()
        .path(item.menu_path.as_str())
        .unwrap()
        .build()
        .await
    {
        Ok(p) => p,
        Err(e) => {
            log::debug!("tray: menu proxy failed for {}: {e}", item.id);
            return Vec::new();
        }
    };

    let layout = match proxy.get_layout(0, 1, &[]).await {
        Ok((_rev, layout)) => layout,
        Err(e) => {
            log::debug!("tray: GetLayout failed for {}: {e}", item.id);
            return Vec::new();
        }
    };

    // layout is (id, properties, children)
    let (_id, _props, children) = layout;
    let mut items = Vec::new();
    for child_val in children {
        if let Ok((child_id, child_props, _)) =
            <(i32, HashMap<String, zbus::zvariant::OwnedValue>, Vec<zbus::zvariant::OwnedValue>)
             as TryFrom<zbus::zvariant::OwnedValue>>::try_from(child_val)
        {
            let label = child_props.get("label")
                .and_then(|v| String::try_from(v.clone()).ok())
                .unwrap_or_default()
                .replace('_', ""); // Remove mnemonic underscores

            let enabled = child_props.get("enabled")
                .and_then(|v| bool::try_from(v.clone()).ok())
                .unwrap_or(true);

            let item_type = child_props.get("type")
                .and_then(|v| String::try_from(v.clone()).ok())
                .unwrap_or_default();

            let is_separator = item_type == "separator";

            // Skip invisible items
            let visible = child_props.get("visible")
                .and_then(|v| bool::try_from(v.clone()).ok())
                .unwrap_or(true);
            if !visible { continue; }

            items.push(MenuItem {
                id: child_id,
                label,
                enabled,
                is_separator,
            });
        }
    }
    items
}

/// Activate a menu item by sending a DBusMenu Event (blocking, call from background thread).
pub fn activate_menu_item(item: &TrayItem, menu_id: i32) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    let rt = match rt {
        Ok(r) => r,
        Err(_) => return,
    };

    rt.block_on(async {
        let conn = match Connection::session().await {
            Ok(c) => c,
            Err(_) => return,
        };

        let proxy = match DbusmenuProxyProxy::builder(&conn)
            .destination(item.service.as_str())
            .unwrap()
            .path(item.menu_path.as_str())
            .unwrap()
            .build()
            .await
        {
            Ok(p) => p,
            Err(_) => return,
        };

        let _ = proxy.event(
            menu_id,
            "clicked",
            &zbus::zvariant::Value::I32(0),
            0,
        ).await;
    });
}

/// Left-click activate a tray item by service name (blocking, call from any thread).
pub fn activate_item_by_service(service: &str, _id: &str) {
    let service = service.to_string();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build();
    let rt = match rt {
        Ok(r) => r,
        Err(_) => return,
    };
    rt.block_on(async {
        let conn = match Connection::session().await {
            Ok(c) => c,
            Err(_) => return,
        };
        if let Ok(proxy) = StatusNotifierItemProxy::builder(&conn)
            .destination(service.as_str())
            .unwrap()
            .path("/StatusNotifierItem")
            .unwrap()
            .build()
            .await
        {
            let _ = proxy.activate(0, 0).await;
        }
    });
}
