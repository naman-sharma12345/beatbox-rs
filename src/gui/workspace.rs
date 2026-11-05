//! Movable studio panels. Layout data is separate from the musical project.
use super::*;
use egui_dock::{DockArea, DockState, NodeIndex, Style, SurfaceIndex, TabViewer};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Pane {
    Sequencer,
    Mixer,
    Automation,
    Playlist,
    Vocal,
    Browser,
    Rack,
    Library,
    Inspector,
    Notes,
    Console,
    Transport,
    Analysis,
    History,
}
impl Pane {
    const ALL: [Self; 14] = [
        Self::Sequencer,
        Self::Mixer,
        Self::Automation,
        Self::Playlist,
        Self::Vocal,
        Self::Browser,
        Self::Rack,
        Self::Library,
        Self::Inspector,
        Self::Notes,
        Self::Console,
        Self::Transport,
        Self::Analysis,
        Self::History,
    ];
    fn title(self) -> &'static str {
        match self {
            Self::Sequencer => "Channel rack",
            Self::Mixer => "Mixer",
            Self::Automation => "Automation",
            Self::Playlist => "Playlist",
            Self::Vocal => "Vocal",
            Self::Browser => "Browser",
            Self::Rack => "FX rack",
            Self::Library => "Library",
            Self::Inspector => "Channel",
            Self::Notes => "Piano roll",
            Self::Console => "Console",
            Self::Transport => "Transport",
            Self::Analysis => "Analysis",
            Self::History => "History",
        }
    }
    /// The FL Studio style key that opens this panel, if it has one.
    fn key_hint(self) -> Option<&'static str> {
        match self {
            Self::Playlist => Some("F5"),
            Self::Sequencer => Some("F6"),
            Self::Notes => Some("F7"),
            Self::Browser => Some("Alt+F8"),
            Self::Mixer => Some("F9"),
            _ => None,
        }
    }
    /// Musical editing surfaces, not the sound-source column or utility panels.
    fn is_central_editor(self) -> bool {
        matches!(
            self,
            Self::Sequencer
                | Self::Playlist
                | Self::Mixer
                | Self::Notes
                | Self::Automation
                | Self::Vocal
                | Self::Rack
        )
    }
    fn view(self) -> Option<usize> {
        Self::ALL[..7].iter().position(|p| *p == self)
    }
}
#[derive(Serialize, Deserialize)]
struct Saved {
    version: u32,
    dock: DockState<Pane>,
    // Missing in v1 preferences: retain the user's legacy docked transport.
    #[serde(default)]
    pinned_transport: bool,
    #[serde(default)]
    mode: u8,
}
pub struct Workspace {
    dock: DockState<Pane>,
    saved: String,
    changed_at: f64,
    path: PathBuf,
    requested: Option<Pane>,
    /// No saved layout yet: rebuild the default once the real window size is known.
    fresh: bool,
    pinned_transport: bool,
    /// Last workflow tab picked in the header: 0 custom, 1 Pattern, 2 Song, 3 Mix.
    mode: u8,
    /// Ctrl+Shift+H: put the windows back to the default arrangement.
    reset: bool,
}
/// Fractions describe the left/top share in egui_dock 0.14's rendered tree.
/// Keep the compact fallback bounded, even before the first real viewport arrives.
fn studio_regions(width: f32, height: f32, pattern: bool) -> (Option<f32>, Option<f32>) {
    if !width.is_finite() || !height.is_finite() || width < 1000.0 || height < 1.0 {
        return (None, None);
    }
    let library = if width < 1300.0 { 216.0 } else { 240.0 };
    let bottom = if pattern && height >= 650.0 {
        let pixels = if height < 800.0 { 240.0 } else { 310.0 };
        Some(1.0 - pixels / (height - 88.0))
    } else {
        None
    };
    (Some(library / width), bottom)
}
/// Prefer independent native viewport metadata. Some backends cannot report
/// an inner position/rect: after the initial pass, an explicitly supplied raw
/// screen rect is sufficient. Never use InputState's retained default rectangle
/// or the first pass's pending-zoom override as that fallback.
fn initial_viewport_size(ctx: &egui::Context) -> Option<Vec2> {
    let past_initial_pass = ctx.cumulative_pass_nr() > 0;
    ctx.input(|i| {
        if let Some(rect) = i.viewport().inner_rect {
            Some(rect.size())
        } else if past_initial_pass {
            i.raw.screen_rect.map(|r| r.size())
        } else {
            None
        }
    })
}

impl Workspace {
    fn default_dock(view: usize, screen_h: f32) -> DockState<Pane> {
        // Leave room for wrapped transport controls as well as the tab header.
        let top = (152.0 / screen_h.max(1.0)).clamp(0.155, 0.4);
        let mut d = DockState::new(vec![Pane::ALL[view.min(6)]]);
        d.main_surface_mut()
            .split_above(NodeIndex::root(), top, vec![Pane::Transport]);
        d
    }
    /// Guard only the standard top strip. Floating and custom dock arrangements stay alone.
    fn keep_transport_visible(dock: &mut DockState<Pane>, height: f32) {
        let main = dock.main_surface_mut();
        if main.len() < 3 || main[NodeIndex(1)].tabs() != Some(&[Pane::Transport][..]) {
            return;
        }
        if let egui_dock::Node::Vertical { fraction, .. } = &mut main[NodeIndex::root()] {
            let min_fraction = (152.0 / height.max(1.0)).min(0.4);
            *fraction = fraction.max(min_fraction);
        }
    }
    /// Beat: transport, step sequencer and piano roll.
    fn beat_dock(h: f32) -> DockState<Pane> {
        let mut d = Self::default_dock(0, h);
        d.main_surface_mut()
            .split_below(NodeIndex(2), 0.48, vec![Pane::Notes]);
        d
    }
    /// Arrange: transport and the song timeline.
    fn arrange_dock(h: f32) -> DockState<Pane> {
        Self::default_dock(3, h)
    }
    /// Mix: transport, mixer, channel and master analysis.
    fn mix_dock(h: f32) -> DockState<Pane> {
        let mut d = Self::default_dock(1, h);
        d.main_surface_mut()
            .split_right(NodeIndex(2), 0.73, vec![Pane::Inspector, Pane::Analysis]);
        d
    }
    /// New presentation only. No project, selection, history or playback access.
    fn studio_dock(view: usize, width: f32, height: f32) -> DockState<Pane> {
        let mut d = DockState::new(vec![Pane::ALL[view.min(6)]]);
        let (library, bottom) = studio_regions(width, height, view == 0);
        let Some(library) = library else {
            return d;
        };
        let [editor, _] = d.main_surface_mut().split_left(
            NodeIndex::root(),
            library,
            vec![Pane::Library, Pane::Browser],
        );
        if let Some(bottom) = bottom {
            d.main_surface_mut()
                .split_below(editor, bottom, vec![Pane::Notes]);
        }
        d
    }
    fn select_mode(&mut self, mode: u8, size: Vec2) {
        self.mode = mode;
        let view = match mode {
            1 => 0,
            2 => 3,
            _ => 1,
        };
        self.dock = if self.pinned_transport {
            Self::studio_dock(view, size.x, size.y)
        } else {
            match mode {
                1 => Self::beat_dock(size.y),
                2 => Self::arrange_dock(size.y),
                _ => Self::mix_dock(size.y),
            }
        };
    }
    /// Automated captures never restore or save a person's interactive layout.
    pub fn capture(ctx: &egui::Context, view: usize) -> Self {
        Self {
            dock: Self::studio_dock(view, ctx.screen_rect().width(), ctx.screen_rect().height()),
            saved: String::new(),
            changed_at: 0.0,
            path: PathBuf::new(),
            requested: None,
            fresh: true,
            pinned_transport: true,
            mode: 0,
            reset: false,
        }
    }
    /// egui applies pending zoom at begin_pass and may substitute the previous
    /// (initially 10000-point) screen rect. Do not freeze that synthetic size into
    /// dock fractions. Native inner_rect is supplied independently by egui-winit.
    /// Only a fresh, unsaved layout is eligible; later resizes never rebuild it.
    fn initialize_viewport(&mut self, view: usize, screen: Vec2, inner: Option<Vec2>) -> bool {
        if !self.fresh || !screen.is_finite() || screen.x <= 0.0 || screen.y <= 0.0 {
            return false;
        }
        let Some(inner) = inner.filter(|v| v.is_finite() && v.x > 0.0 && v.y > 0.0) else {
            return false;
        };
        if (screen - inner).abs().max_elem() > 1.0 {
            return false;
        }
        self.fresh = false;
        if self.path.as_os_str().is_empty() {
            self.rebuild_capture(view, screen);
        } else {
            self.select_mode(1, screen);
        }
        true
    }
    fn rebuild_capture(&mut self, view: usize, size: Vec2) {
        self.dock = Self::studio_dock(view, size.x, size.y);
        self.mode = match view {
            0 => 1,
            3 => 2,
            1 => 3,
            _ => 0,
        };
    }
    /// Presentation only, after capture's first real viewport rebuild.
    fn seed_capture_presentation(&mut self, inspector: bool) {
        if inspector {
            self.open(Pane::Inspector);
        }
    }
    /// Rectangles are populated by DockArea, never reconstructed from fractions.
    pub(super) fn capture_presentation(&self, inspector: bool) -> anyhow::Result<Value> {
        let mut panes = Vec::new();
        let mut central = Rect::NOTHING;
        let mut channel = None;
        let mut count = 0;
        for ((surface, node), pane) in self.dock.iter_all_tabs() {
            let rect = self.dock[surface][node]
                .rect()
                .ok_or_else(|| anyhow::anyhow!("missing pane rectangle"))?;
            anyhow::ensure!(
                rect.is_finite() && rect.is_positive(),
                "invalid observed pane rectangle"
            );
            if surface == SurfaceIndex::main() {
                anyhow::ensure!(
                    *pane != Pane::Library || rect.width() >= 200.0,
                    "capture Library is below its useful desktop width"
                );
                anyhow::ensure!(
                    *pane != Pane::Notes || self.mode != 1 || rect.height() >= 220.0,
                    "capture Notes is below its useful Pattern height"
                );
            }
            if pane.is_central_editor() && surface == SurfaceIndex::main() {
                central = central.union(rect);
            }
            if *pane == Pane::Inspector {
                count += 1;
                channel = Some((surface, rect));
            }
            panes.push(json!({"pane":format!("{pane:?}"),"label":pane.title(),
                "main_surface":surface==SurfaceIndex::main(),
                "rect":[rect.min.x,rect.min.y,rect.max.x,rect.max.y]}));
        }
        anyhow::ensure!(
            self.pinned_transport && self.mode != 0 && central.is_finite() && central.is_positive(),
            "invalid capture workspace"
        );
        let right = channel.is_some_and(|(surface, r)| {
            surface == SurfaceIndex::main()
                && r.min.x >= central.max.x
                && r.min.y <= central.min.y + 1.1
                && r.max.y >= central.max.y - 1.1
        });
        anyhow::ensure!(
            if inspector {
                count == 1 && right
            } else {
                count == 0
            },
            "Inspector must be unique and right of entire central subtree"
        );
        Ok(
            json!({"mode":self.mode,"pinned_transport":self.pinned_transport,"panes":panes,
            "central_rect":[central.min.x,central.min.y,central.max.x,central.max.y],
            "inspector_count":count,"inspector_right_of_entire_central":right}),
        )
    }
    pub fn load(ctx: &egui::Context, view: usize) -> Self {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_else(std::env::temp_dir);
        let path = base.join("beatbox/studio-layout-v1.json");
        let text = std::fs::read_to_string(&path).ok();
        let fresh = text.is_none();
        let pinned_transport = fresh
            || text
                .as_deref()
                .and_then(|s| serde_json::from_str::<Saved>(s).ok())
                .filter(|s| s.version == 1)
                .is_some_and(|s| s.pinned_transport);
        let dock = Self::restore(text.as_deref(), view, ctx.screen_rect().shrink(12.0));
        Self {
            dock,
            saved: String::new(),
            changed_at: 0.0,
            path,
            requested: None,
            fresh,
            pinned_transport,
            mode: if fresh {
                1
            } else {
                text.as_deref()
                    .and_then(|s| serde_json::from_str::<Saved>(s).ok())
                    .filter(|s| s.version == 1 && s.mode <= 3)
                    .map_or(0, |s| s.mode)
            },
            reset: false,
        }
    }
    fn restore(text: Option<&str>, view: usize, bounds: Rect) -> DockState<Pane> {
        let mut dock = text
            .and_then(|s| {
                let mut v: Value = serde_json::from_str(s).ok()?;
                if let Some(a) = v["dock"]["surfaces"].as_array_mut() {
                    for s in a {
                        if let Some(w) = s.get_mut("Window") {
                            w[1]["new"] = json!(true);
                        }
                    }
                }
                serde_json::from_value::<Saved>(v).ok()
            })
            .filter(|s| s.version == 1)
            .map(|s| s.dock)
            .unwrap_or_else(|| Self::default_dock(view, bounds.height() + 24.0));
        for i in 1..dock.surfaces_count() {
            let si = egui_dock::SurfaceIndex(i);
            let rect = dock
                .get_surface(si)
                .and_then(|s| s.node_tree())
                .and_then(|tree| tree.iter().next())
                .and_then(|node| node.rect());
            if let Some(r) = rect {
                let size = (r.size() + Vec2::new(12.0, 44.0))
                    .max(Vec2::new(440.0, 360.0))
                    .min(bounds.size());
                let pos = Pos2::new(
                    r.left()
                        .clamp(bounds.left(), (bounds.right() - size.x).max(bounds.left())),
                    (r.top() - 32.0).clamp(60.0, (bounds.bottom() - size.y).max(60.0)),
                );
                if let Some(w) = dock.get_window_state_mut(si) {
                    w.set_position(pos).set_size(size);
                }
            }
        }
        dock
    }
    pub(super) fn request_view(&mut self, v: usize) {
        self.requested = Some(Pane::ALL[v.min(6)]);
    }
    pub fn request_pane(&mut self, p: Pane) {
        self.requested = Some(p);
    }
    pub fn request_reset(&mut self) {
        self.reset = true;
    }
    /// Recompute roles after every close/move: node indices are not persistent anchors.
    /// Prefer the preset's primary on the main surface, then a focused/editor leaf.
    /// Floating secondary views never become the default destination of a new panel.
    fn central_leaf(&self) -> Option<NodeIndex> {
        let main = self.dock.main_surface();
        let usable = |n: NodeIndex| {
            n.0 < main.len()
                && main[n].tabs().is_some_and(|tabs| {
                    !tabs.is_empty()
                        && !tabs
                            .iter()
                            .any(|p| matches!(p, Pane::Library | Pane::Browser | Pane::Transport))
                })
        };
        let preferred = match self.mode {
            1 => Some(Pane::Sequencer),
            2 => Some(Pane::Playlist),
            3 => Some(Pane::Mixer),
            _ => None,
        };
        if let Some((n, _)) = preferred.and_then(|p| self.dock.find_main_surface_tab(&p)) {
            if usable(n) {
                return Some(n);
            }
        }
        if let Some((s, n)) = self.dock.focused_leaf() {
            if s == SurfaceIndex::main()
                && usable(n)
                && main[n]
                    .tabs()
                    .unwrap()
                    .iter()
                    .any(|p| p.is_central_editor())
            {
                return Some(n);
            }
        }
        for pane in [
            Pane::Sequencer,
            Pane::Playlist,
            Pane::Mixer,
            Pane::Notes,
            Pane::Automation,
            Pane::Vocal,
            Pane::Rack,
        ] {
            if let Some((n, _)) = self.dock.find_main_surface_tab(&pane) {
                if usable(n) {
                    return Some(n);
                }
            }
        }
        // The last editor may have closed, leaving a central utility leaf. Reuse it,
        // but never the library, transport or a floating window, even if focused.
        main.iter()
            .enumerate()
            .find_map(|(i, _)| usable(NodeIndex(i)).then_some(NodeIndex(i)))
    }
    /// Largest editor-only ancestor of this leaf. In Pattern this is the full
    /// rack/piano stack. Stop at any library or utility sibling in a custom layout:
    /// do not reposition user panels just to recreate the preset's silhouette.
    fn central_region(&self, leaf: NodeIndex) -> NodeIndex {
        let mut region = leaf;
        while let Some(parent) = region.parent() {
            let mut any = false;
            let only_editors = self
                .dock
                .main_surface()
                .iter()
                .enumerate()
                .all(|(i, node)| {
                    let mut ancestor = Some(NodeIndex(i));
                    while let Some(n) = ancestor {
                        if n == parent {
                            return node.tabs().is_none_or(|tabs| {
                                any |= !tabs.is_empty();
                                tabs.iter().all(|p| p.is_central_editor())
                            });
                        }
                        ancestor = n.parent();
                    }
                    true
                });
            if !any || !only_editors {
                break;
            }
            region = parent;
        }
        region
    }
    fn open_pinned(&mut self, p: Pane) {
        // A deserialized zero-surface dock has nothing to preserve or index.
        if self.dock.surfaces_count() == 0 {
            self.dock = DockState::new(vec![p]);
            self.dock
                .set_focused_node_and_surface((SurfaceIndex::main(), NodeIndex::root()));
            return;
        }
        let main = SurfaceIndex::main();
        // Explicitly seed an empty main tree; do not push through stale floating focus.
        if self
            .dock
            .main_surface()
            .iter()
            .all(|node| node.tabs().is_none_or(|tabs| tabs.is_empty()))
        {
            self.dock.main_surface_mut().push_to_first_leaf(p);
        } else if matches!(p, Pane::Library | Pane::Browser) {
            let library = self
                .dock
                .find_main_surface_tab(&Pane::Library)
                .or_else(|| self.dock.find_main_surface_tab(&Pane::Browser));
            if let Some((n, _)) = library {
                self.dock.main_surface_mut()[n].append_tab(p);
            } else {
                self.dock
                    .main_surface_mut()
                    .split_left(NodeIndex::root(), 0.22, vec![p]);
            }
        } else if let Some(leaf) = self.central_leaf() {
            if p == Pane::Inspector {
                let region = self.central_region(leaf);
                // 32% of the centre leaves a usable inspector at both desktop sizes.
                // Tree::split_right supports parent nodes and preserves their subtree.
                self.dock
                    .main_surface_mut()
                    .split_right(region, 0.68, vec![p]);
            } else {
                self.dock.main_surface_mut()[leaf].append_tab(p);
            }
        } else {
            // Only Library/Browser/Transport remain. Keep them intact on the left,
            // restoring a central region without cloning or rebuilding other panes.
            self.dock
                .main_surface_mut()
                .split_right(NodeIndex::root(), 0.22, vec![p]);
        }
        if let Some((s, n, t)) = self.dock.find_tab(&p) {
            debug_assert_eq!(s, main);
            self.dock[s].set_active_tab(n, t);
            self.dock.set_focused_node_and_surface((s, n));
        }
    }
    fn open(&mut self, p: Pane) {
        if let Some((s, n, t)) = self.dock.find_tab(&p) {
            self.dock[s].set_active_tab(n, t);
            self.dock.set_focused_node_and_surface((s, n));
        } else if self.pinned_transport {
            self.open_pinned(p);
        } else {
            // Legacy/custom v1 behavior is deliberately unchanged.
            // Reopening an editor must not bury it in the short Transport strip.
            let target = self
                .dock
                .iter_all_tabs()
                .find(|(_, tab)| tab.view().is_some())
                .map(|((s, n), _)| (s, n));
            if let (Pane::Browser, Some((s, n))) = (p, target) {
                // FL keeps its browser in a full-height column on the left: you pick a
                // sound while still looking at the channels it will land in
                // below the transport row, so Undo, Redo and Save keep their room
                let mut region = n;
                while let Some(up) = region.parent().filter(|u| *u != NodeIndex::root()) {
                    region = up;
                }
                let [_, fresh] = self.dock[s].split_left(region, 0.42, vec![p]);
                self.dock.set_focused_node_and_surface((s, fresh));
                return;
            }
            if let Some(t) = target {
                self.dock.set_focused_node_and_surface(t);
            }
            self.dock.push_to_focused_leaf(p);
        }
    }
    pub(super) fn flush(&self) -> std::io::Result<()> {
        if self.path.as_os_str().is_empty() {
            return Ok(());
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut value = serde_json::to_value(Saved {
            version: 1,
            dock: self.dock.clone(),
            pinned_transport: self.pinned_transport,
            mode: self.mode,
        })?;
        fn finite(v: &mut Value) {
            match v {
                Value::Object(m) => {
                    for (k, v) in m {
                        if (k == "x" || k == "y") && v.is_null() {
                            *v = json!(0.0);
                        } else {
                            finite(v);
                        }
                    }
                }
                Value::Array(a) => {
                    for v in a {
                        finite(v)
                    }
                }
                _ => {}
            }
        }
        finite(&mut value);
        let bytes = serde_json::to_vec(&value)?;
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(tmp, &self.path)
    }
    fn persist(&mut self, now: f64) {
        if let Ok(s) = serde_json::to_string(&Saved {
            version: 1,
            dock: self.dock.clone(),
            pinned_transport: self.pinned_transport,
            mode: self.mode,
        }) {
            if s != self.saved {
                self.saved = s;
                self.changed_at = now;
            }
            if self.changed_at > 0.0 && now - self.changed_at > 0.8 {
                if self.flush().is_ok() {
                    self.changed_at = 0.0;
                } else {
                    self.changed_at = now;
                }
            }
        }
    }
}
struct Viewer<'a> {
    studio: &'a mut Studio,
    p: &'a Project,
    rev: u64,
    log: &'a (Vec<String>, Vec<String>),
    undo: (usize, usize),
    request: Option<Pane>,
    pinned_transport: bool,
}
impl TabViewer for Viewer<'_> {
    type Tab = Pane;
    fn title(&mut self, p: &mut Pane) -> egui::WidgetText {
        egui::RichText::new(p.title()).size(12.0).into()
    }
    fn id(&mut self, p: &mut Pane) -> egui::Id {
        egui::Id::new(("studio-panel", *p))
    }
    fn on_tab_button(&mut self, p: &mut Pane, r: &egui::Response) {
        if r.clicked() {
            if let Some(v) = p.view() {
                self.studio.view = v;
            }
        }
    }
    fn scroll_bars(&self, _: &Pane) -> [bool; 2] {
        [true, true]
    }
    fn ui(&mut self, ui: &mut egui::Ui, pane: &mut Pane) {
        ui.set_min_size(match pane {
            Pane::Transport => Vec2::new(420.0, 78.0),
            Pane::Notes if self.pinned_transport => Vec2::new(420.0, 200.0),
            Pane::Sequencer if self.pinned_transport => Vec2::new(420.0, 220.0),
            Pane::Console => Vec2::new(420.0, 230.0),
            Pane::Library | Pane::Browser => Vec2::new(184.0, 220.0),
            Pane::Inspector | Pane::Analysis | Pane::History => Vec2::new(250.0, 220.0),
            _ => Vec2::new(420.0, 320.0),
        });
        let p = self.p;
        let s = &mut self.studio;
        if let Some(v) = pane.view() {
            if ui.rect_contains_pointer(ui.max_rect()) {
                s.view = v;
            }
        }
        ui.push_id(*pane, |ui| match pane {
            Pane::Transport if self.pinned_transport => {
                ui.label("Transport is pinned above the workspace.");
                ui.label("Use Layout > Legacy transport layout for a movable transport.");
            }
            Pane::Transport => {
                egui::ScrollArea::both()
                    .id_salt("transport")
                    .auto_shrink([false, false])
                    .show(ui, |ui| s.top_bar(ui, p, self.undo));
            }
            Pane::Library => s.browser(ui, p),
            Pane::Inspector => s.inspector(ui, p, self.rev, self.log),
            Pane::Console => s.console_dock(ui, p),
            Pane::Notes => s.piano_roll(ui, p),
            Pane::Analysis => {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| s.master(ui, p));
            }
            Pane::History => {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.label(RichText::new("Project history").size(20.0).strong());
                        ui.add_space(12.0);
                        for (i, line) in self.log.1.iter().enumerate() {
                            if ui
                                .selectable_label(false, RichText::new(line).color(DIM).italics())
                                .on_hover_text("Redo up to here")
                                .clicked()
                            {
                                s.call("redo", json!({"steps":self.log.1.len()-i}));
                            }
                        }
                        for (i, line) in self.log.0.iter().enumerate() {
                            if ui
                                .selectable_label(i == 0, line)
                                .on_hover_text("Go back to this step")
                                .clicked()
                                && i > 0
                            {
                                s.call("undo", json!({"steps":i}));
                            }
                        }
                    });
            }
            _ => {
                if p.tracks.is_empty()
                    && !matches!(
                        pane,
                        Pane::Browser | Pane::Automation | Pane::Vocal | Pane::Mixer
                    )
                {
                    ui.add_space(8.0);
                    ui.label(RichText::new("Start a pattern").size(17.0).strong());
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new("Add a sound, then write drums or notes below.").color(DIM),
                    );
                    ui.add_space(18.0);
                    ui.horizontal_wrapped(|ui| {
                        if ui.button("Add drum kit").clicked() {
                            for n in ["kick", "snare", "hat"] {
                                s.call("add_track", json!({"name":n,"preset":n}));
                            }
                        }
                        if ui.button("Add synth").clicked() {
                            s.call("add_track", json!({"name":"synth","preset":"supersaw"}));
                        }
                        if ui.button("Browse sounds").clicked() {
                            self.request = Some(Pane::Browser);
                        }
                    });
                    ui.add_space(12.0);
                }
                match pane {
                    Pane::Sequencer => {
                        s.arrangement(ui, p);
                        ui.add_space(12.0);
                        s.sequencer(ui, p);
                    }
                    Pane::Mixer => s.mixer(ui, p),
                    Pane::Rack => s.rack_view(ui, p, self.rev),
                    Pane::Playlist => s.playlist_view(ui, p, self.rev, self.pinned_transport),
                    Pane::Automation | Pane::Vocal | Pane::Browser => {
                        egui::ScrollArea::vertical()
                            .id_salt("content")
                            .auto_shrink([false, false])
                            .show(ui, |ui| match pane {
                                Pane::Vocal => s.vocal_view(ui, p),
                                Pane::Browser => s.browser_view(ui, p),
                                _ => s.automation_view(ui, p),
                            });
                    }
                    _ => {}
                }
            }
        });
    }
}
impl Studio {
    pub(super) fn workspace_ui(
        &mut self,
        ctx: &egui::Context,
        p: &Project,
        rev: u64,
        log: &(Vec<String>, Vec<String>),
        undo: (usize, usize),
    ) {
        let mut ws = std::mem::replace(
            &mut self.workspace,
            Workspace {
                dock: DockState::new(vec![]),
                saved: String::new(),
                changed_at: 0.0,
                path: PathBuf::new(),
                requested: None,
                fresh: false,
                pinned_transport: false,
                mode: 0,
                reset: false,
            },
        );
        let bounds = ctx.screen_rect().shrink(12.0);
        let inner_size = initial_viewport_size(ctx);
        if ws.initialize_viewport(self.view, ctx.screen_rect().size(), inner_size)
            && ws.path.as_os_str().is_empty()
        {
            if let Some(capture) = &mut self.p0a_capture {
                if !capture.seeded {
                    ws.seed_capture_presentation(capture.config.inspector);
                    capture.seeded = true;
                }
            }
        }
        if std::mem::take(&mut ws.reset) {
            if ws.pinned_transport {
                ws.select_mode(1, ctx.screen_rect().size());
            } else {
                ws.mode = 0;
                ws.dock = Workspace::default_dock(self.view, ctx.screen_rect().height());
            }
        }
        if let Some(p) = ws.requested.take() {
            ws.open(p);
        }
        egui::TopBottomPanel::top("workspace_header")
            .exact_height(36.0)
            .frame(
                egui::Frame::none()
                    .fill(BG)
                    .inner_margin(egui::Margin::symmetric(12.0, 4.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("beatbox").size(14.0).strong());
                    ui.separator();
                    let h = ui.ctx().screen_rect().height();
                    for (m, name, tip) in [
                        (
                            1u8,
                            "Pattern",
                            "Write drums and melodies: step sequencer and piano roll",
                        ),
                        (2u8, "Song", "Lay patterns out into a song"),
                        (3u8, "Mix", "Balance levels, effects and the master"),
                    ] {
                        let on = ws.mode == m;
                        let label = RichText::new(name).size(14.0).color(if on {
                            TEXT
                        } else {
                            DIM
                        });
                        let mut btn = egui::Button::new(label);
                        btn = if on {
                            btn.fill(theme::Tokens::DARK.accent_dark)
                                .stroke(egui::Stroke::new(1.0_f32, ACCENT))
                        } else {
                            btn.frame(false)
                        };
                        if ui.add(btn).on_hover_text(tip).clicked() {
                            ws.select_mode(m, ctx.screen_rect().size());
                        }
                    }
                    if ws.mode == 0 { ui.label(RichText::new("Custom layout").size(12.0).color(DIM)); }
                    ui.add_space(12.0);
                    ui.menu_button("Panels", |ui| {
                        let pinned_transport = ws.pinned_transport;
                        let mut toggle = |ui: &mut egui::Ui, pane: Pane| {
                            let open = ws.dock.find_tab(&pane).is_some();
                            let mut visible = open;
                            if ui
                                .checkbox(
                                    &mut visible,
                                    match pane.key_hint() {
                                        Some(k) => format!("{}   ({k})", pane.title()),
                                        None => pane.title().to_string(),
                                    },
                                )
                                .changed()
                            {
                                if open {
                                    if let Some(loc) = ws.dock.find_tab(&pane) {
                                        ws.dock.remove_tab(loc);
                                    }
                                } else {
                                    ws.open(pane);
                                }
                                ui.close_menu();
                            }
                        };
                        let main = [
                            Pane::Sequencer,
                            Pane::Notes,
                            Pane::Playlist,
                            Pane::Mixer,
                            Pane::Browser,
                        ];
                        for pane in main {
                            toggle(ui, pane);
                        }
                        ui.menu_button("More panels", |ui| {
                            for pane in Pane::ALL {
                                if !main.contains(&pane) && !(pinned_transport && pane == Pane::Transport) {
                                    toggle(ui, pane);
                                }
                            }
                        });
                    });
                    ui.menu_button("Layout", |ui| {
                        if ui.button("Graphite workspace (Pattern)").on_hover_text("Replace only this UI layout. Music and history are unchanged.").clicked() {
                            ws.pinned_transport = true;
                            ws.select_mode(1, ctx.screen_rect().size());
                            ui.close_menu();
                        }
                        if ui.button("Legacy transport layout").clicked() {
                            ws.pinned_transport = false;
                            ws.mode = 0;
                            ws.dock = Workspace::default_dock(self.view, h);
                            ui.close_menu();
                        }
                        if ui.button("Reset workspace").clicked() {
                            if ws.pinned_transport { ws.select_mode(1, ctx.screen_rect().size()); }
                            else { ws.mode = 0; ws.dock = Workspace::default_dock(self.view, h); }
                            ui.close_menu();
                        }
                        if ui.button("Focus on one panel").clicked() {
                            ws.mode = 0;
                            ws.dock = DockState::new(vec![Pane::ALL[self.view.min(6)]]);
                            ui.close_menu();
                        }
                        ui.separator();
                        ui.label(
                            RichText::new("Drag tabs to split, group or float panels.").size(12.0),
                        );
                        ui.label(
                            RichText::new("Drag dividers to resize. Layout saves automatically.")
                                .size(12.0),
                        );
                    });
                    ui.menu_button("Help", |ui| {
                        ui.set_max_width(420.0);
                        ui.heading("Studio guide");
                        ui.label("Pattern: write drums in the channel rack and melodies in the piano roll.");
                        ui.label("Song: place patterns and edit clips on the song timeline.");
                        ui.label("Mix: balance tracks, add effects and check the master.");
                        ui.separator();
                        ui.menu_button("Patterns, song and recording", |ui| {
                            ui.set_max_width(380.0);
                            ui.label("The channel rack and piano roll edit the selected pattern. Selecting a pattern does not place it in the song.");
                            ui.label("Arrange > PATTERN GRID places patterns on the timeline. Right-click a clip for edit commands.");
                            ui.label("PERFORM launches a pattern on the next bar. SONG returns to the arrangement.");
                            ui.label("PERFORM Rec records those launches, not microphone audio. Press Play, Rec, then launch patterns. Place take layers the result over the song; one Undo removes it.");
                            ui.label("Do not edit the project during a launch take. If anything changes, discard it and record again.");
                            ui.label("The round record button in Transport records audio from your input instead.");
                        });
                        ui.menu_button("Keyboard shortcuts", |ui| {
                            for (keys, description) in shortcuts::help_rows(cfg!(target_os = "macos")) {
                                ui.horizontal(|ui| {
                                    ui.monospace(keys);
                                    ui.label(description);
                                });
                            }
                            ui.separator();
                            ui.label("Shortcuts pause while you type in a text field.");
                        });
                        ui.separator();
                        ui.label("Missing a panel? Open Panels. Drag tabs to split or float them. Layout > Reset workspace restores the default.");
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let state = save_state(self.project_path.is_some(), rev, self.saved_rev);
                        ui.label(RichText::new(state).color(if state == "Saved" { DIM } else { ACCENT }).size(12.0))
                            .on_hover_text(self.save_tooltip());
                        ui.add(egui::Label::new(RichText::new(&p.name).color(DIM).size(12.0)).truncate())
                            .on_hover_text(&p.name);
                        if self.rendering.is_some() && ui.ctx().screen_rect().width() >= 900.0 {
                            ui.spinner();
                        }
                    });
                });
            });
        if ws.pinned_transport {
            egui::TopBottomPanel::top("pinned_transport")
                .frame(
                    egui::Frame::none()
                        .fill(PANEL)
                        .inner_margin(egui::Margin::symmetric(12.0, 4.0)),
                )
                .show(ctx, |ui| {
                    // Wrap on small windows rather than clipping Stop or Save.
                    self.transport_controls(ui, p, undo, true);
                });
        }
        let active_before = self.view;
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(BG)
                    .inner_margin(egui::Margin::symmetric(8.0, 4.0)),
            )
            .show(ctx, |ui| {
                if !ws.pinned_transport {
                    Workspace::keep_transport_visible(&mut ws.dock, ui.available_height());
                }
                let mut style = Style::from_egui(ui.style());
                style.tab_bar.height = 28.0;
                style.separator.extra = 48.0;
                style.tab_bar.bg_fill = BG;
                style.tab_bar.hline_color = LINE;
                style.tab.tab_body.inner_margin = egui::Margin::same(8.0);
                style.tab.tab_body.bg_fill = PANEL;
                style.separator.width = 4.0;
                style.separator.color_idle = BG;
                style.separator.color_hovered = ACCENT;
                style.separator.color_dragged = ACCENT;
                style.tab.active.bg_fill = PANEL2;
                style.tab_bar.rounding = egui::Rounding::same(6.0);
                style.tab.focused.bg_fill = PANEL2;
                style.tab.inactive.bg_fill = BG;
                style.tab.inactive.text_color = DIM;
                style.tab.active.text_color = TEXT;
                style.tab.focused.text_color = TEXT;
                let mut viewer = Viewer {
                    studio: self,
                    p,
                    rev,
                    log,
                    undo,
                    request: None,
                    pinned_transport: ws.pinned_transport,
                };
                DockArea::new(&mut ws.dock)
                    .style(style)
                    .window_bounds(bounds)
                    .show_close_buttons(false)
                    .show_inside(ui, &mut viewer);
                if let Some(p) = viewer.request {
                    ws.requested = Some(p);
                }
            });
        if self.view != active_before {
            ws.open(Pane::ALL[self.view.min(6)]);
        }
        if self.note_editor {
            ws.open(Pane::Notes);
            self.note_editor = false;
        }
        // Never save a placeholder while the native viewport/zoom is settling.
        if !ws.fresh {
            ws.persist(ctx.input(|i| i.time));
        }
        self.workspace = ws;
    }
}

#[cfg(test)]
mod tests {
    // These exercise the real Workspace and egui_dock types in feature-enabled CI.
    // Fraction checks below are nominal layout geometry, not painted GUI evidence.
    fn descendant(mut node: NodeIndex, ancestor: NodeIndex) -> bool {
        loop {
            if node == ancestor {
                return true;
            }
            let Some(parent) = node.parent() else {
                return false;
            };
            node = parent;
        }
    }
    fn unique(w: &Workspace, pane: Pane) {
        assert_eq!(
            w.dock.iter_all_tabs().filter(|(_, p)| **p == pane).count(),
            1
        );
    }
    #[test]
    fn refinement_fresh_waits_for_positive_matching_native_viewport_once() {
        let mut w = workspace();
        w.fresh = true;
        w.pinned_transport = true;
        let before = serde_json::to_value(&w.dock).unwrap();
        for (screen, inner) in [
            (Vec2::ZERO, Some(Vec2::ZERO)),
            (Vec2::new(f32::NAN, 700.0), Some(Vec2::new(1100.0, 700.0))),
            (Vec2::new(1100.0, 700.0), None),
            (Vec2::new(5000.0, 5000.0), Some(Vec2::new(2200.0, 1400.0))),
            (
                Vec2::new(1100.0, 700.0),
                Some(Vec2::new(f32::INFINITY, 700.0)),
            ),
        ] {
            assert!(!w.initialize_viewport(0, screen, inner));
            assert!(w.fresh);
            assert_eq!(before, serde_json::to_value(&w.dock).unwrap());
        }
        let size = Vec2::new(1100.0, 700.0);
        assert!(w.initialize_viewport(0, size, Some(size)));
        assert!(!w.fresh);
        assert_eq!(w.mode, 1);
        let after = serde_json::to_value(&w.dock).unwrap();
        assert!(!w.initialize_viewport(
            3,
            Vec2::new(1440.0, 900.0),
            Some(Vec2::new(1440.0, 900.0))
        ));
        assert_eq!(after, serde_json::to_value(&w.dock).unwrap());
    }

    #[test]
    fn refinement_real_dock_first_viewport_geometry_at_1x_and_2x() {
        struct GeometryViewer;
        impl TabViewer for GeometryViewer {
            type Tab = Pane;
            fn title(&mut self, pane: &mut Pane) -> egui::WidgetText {
                pane.title().into()
            }
            fn ui(&mut self, ui: &mut egui::Ui, _: &mut Pane) {
                ui.label("geometry only");
            }
        }
        for scale in [1.0, 2.0] {
            for size in [Vec2::new(1100.0, 700.0), Vec2::new(1440.0, 900.0)] {
                for view in [0, 3, 1] {
                    let ctx = egui::Context::default();
                    ctx.set_pixels_per_point(scale);
                    let mut w = Workspace::capture(&ctx, view);
                    for frame in 0..3 {
                        let native_size = if frame == 0 { size * scale } else { size };
                        let mut input = egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, native_size)),
                            ..Default::default()
                        };
                        let viewport = input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap();
                        viewport.inner_rect = Some(Rect::from_min_size(Pos2::ZERO, native_size));
                        viewport.native_pixels_per_point = Some(1.0);
                        let _ = ctx.run(input, |ctx| {
                            let initialized = w.initialize_viewport(
                                view,
                                ctx.screen_rect().size(),
                                initial_viewport_size(ctx),
                            );
                            if frame == 0 && scale == 2.0 {
                                // Pinned egui's pending zoom overrides the incoming screen
                                // with its default 10000-point rectangle scaled to 5000.
                                assert_eq!(ctx.screen_rect().size(), Vec2::splat(5000.0));
                                assert!(!initialized);
                                assert!(w.fresh);
                            }
                            egui::TopBottomPanel::top("chrome")
                                .exact_height(82.0)
                                .show(ctx, |_| {});
                            egui::CentralPanel::default()
                                .frame(
                                    egui::Frame::none()
                                        .inner_margin(egui::Margin::symmetric(8.0, 4.0)),
                                )
                                .show(ctx, |ui| {
                                    let mut style = Style::from_egui(ui.style());
                                    style.separator.width = 4.0;
                                    style.tab_bar.height = 28.0;
                                    DockArea::new(&mut w.dock)
                                        .style(style)
                                        .show_inside(ui, &mut GeometryViewer);
                                });
                        });
                    }
                    assert!(!w.fresh);
                    let (n, _) = w.dock.find_main_surface_tab(&Pane::Library).unwrap();
                    let library = w.dock.main_surface()[n].rect().unwrap();
                    let expected_library = if size.x < 1300.0 { 216.0 } else { 240.0 };
                    assert!(
                        (library.width() - (expected_library * (size.x - 16.0) / size.x - 2.0))
                            .abs()
                            < 1.1
                    );
                    assert!(library.width() >= 210.0);
                    if view == 0 {
                        let (n, _) = w.dock.find_main_surface_tab(&Pane::Notes).unwrap();
                        let notes = w.dock.main_surface()[n].rect().unwrap();
                        let expected_notes = if size.y < 800.0 { 240.0 } else { 310.0 };
                        assert!(
                            (notes.height()
                                - (expected_notes * (size.y - 90.0) / (size.y - 88.0) - 2.0))
                                .abs()
                                < 1.1
                        );
                        assert!(notes.height() > 230.0);
                    }
                    w.capture_presentation(false).unwrap();
                    let (library_node, _) = w.dock.find_main_surface_tab(&Pane::Library).unwrap();
                    w.dock.main_surface_mut()[library_node].set_rect(Rect::from_min_size(
                        library.min,
                        Vec2::new(50.0, library.height()),
                    ));
                    assert!(w.capture_presentation(false).is_err());
                    w.dock.main_surface_mut()[library_node].set_rect(library);
                    if view == 0 {
                        let (notes_node, _) = w.dock.find_main_surface_tab(&Pane::Notes).unwrap();
                        let notes = w.dock.main_surface()[notes_node].rect().unwrap();
                        w.dock.main_surface_mut()[notes_node].set_rect(Rect::from_min_size(
                            notes.min,
                            Vec2::new(notes.width(), 36.5),
                        ));
                        assert!(w.capture_presentation(false).is_err());
                        w.dock.main_surface_mut()[notes_node].set_rect(notes);
                    }
                    let before = serde_json::to_value(&w.dock).unwrap();
                    let resized = size + Vec2::new(200.0, 100.0);
                    assert!(!w.initialize_viewport(view, resized, Some(resized)));
                    assert_eq!(before, serde_json::to_value(&w.dock).unwrap());
                }
            }
        }
    }

    #[test]
    fn refinement_missing_inner_rect_uses_explicit_screen_only_after_first_pass() {
        let ctx = egui::Context::default();
        ctx.set_pixels_per_point(2.0);
        let mut w = Workspace::capture(&ctx, 0);
        for frame in 0..2 {
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1100.0, 700.0))),
                    ..Default::default()
                },
                |ctx| {
                    let initialized = w.initialize_viewport(
                        0,
                        ctx.screen_rect().size(),
                        initial_viewport_size(ctx),
                    );
                    assert_eq!(initialized, frame == 1);
                },
            );
        }
        assert!(!w.fresh);
    }

    #[test]
    fn refinement_saved_custom_floating_layout_never_reinitializes() {
        let mut w = workspace();
        w.pinned_transport = true;
        w.mode = 0;
        w.dock = DockState::new(vec![Pane::Playlist, Pane::Console]);
        let float = w.dock.add_window(vec![Pane::Notes]);
        w.dock.main_surface_mut()[NodeIndex::root()]
            .set_rect(Rect::from_min_size(Pos2::ZERO, Vec2::new(1100.0, 700.0)));
        w.dock[float][NodeIndex::root()].set_rect(Rect::from_min_size(
            Pos2::new(300.0, 180.0),
            Vec2::new(500.0, 400.0),
        ));
        w.flush().unwrap();
        let saved = std::fs::read_to_string(&w.path).unwrap();
        std::fs::remove_file(&w.path).unwrap();
        let bounds = Rect::from_min_size(Pos2::ZERO, Vec2::new(1100.0, 700.0));
        w.dock = Workspace::restore(Some(&saved), 3, bounds);
        let before = serde_json::to_value(&w.dock).unwrap();
        for size in [
            Vec2::ZERO,
            Vec2::new(1100.0, 700.0),
            Vec2::new(1440.0, 900.0),
        ] {
            assert!(!w.initialize_viewport(0, size, Some(size)));
            assert_eq!(before, serde_json::to_value(&w.dock).unwrap());
        }
        assert_eq!(w.mode, 0);
        unique(&w, Pane::Notes);
        assert_ne!(
            w.dock.find_tab(&Pane::Notes).unwrap().0,
            SurfaceIndex::main()
        );
    }

    #[test]
    fn p0a_seed_preserves_real_workspace_mode_and_is_idempotent() {
        for size in [Vec2::new(1100.0, 700.0), Vec2::new(1440.0, 900.0)] {
            for (mode, primary) in [(1, Pane::Sequencer), (2, Pane::Playlist), (3, Pane::Mixer)] {
                for inspector in [false, true] {
                    let ctx = egui::Context::default();
                    let view = primary.view().unwrap();
                    let mut w = Workspace::capture(&ctx, view);
                    assert!(std::mem::take(&mut w.fresh));
                    assert!(w.path.as_os_str().is_empty());
                    w.rebuild_capture(view, size);
                    w.seed_capture_presentation(inspector);
                    assert_eq!(w.mode, mode);
                    unique(&w, primary);
                    assert_eq!(
                        w.dock
                            .iter_all_tabs()
                            .filter(|(_, p)| **p == Pane::Inspector)
                            .count(),
                        usize::from(inspector)
                    );
                    let before = serde_json::to_value(&w.dock).unwrap();
                    w.seed_capture_presentation(inspector);
                    assert_eq!(before, serde_json::to_value(&w.dock).unwrap());
                }
            }
        }
    }
    #[test]
    fn r1_inspector_is_right_of_entire_central_region_at_both_desktop_sizes() {
        for (width, height, library_width) in [(1100.0, 700.0, 216.0), (1440.0, 900.0, 240.0)] {
            for (mode, primary) in [(1, Pane::Sequencer), (2, Pane::Playlist), (3, Pane::Mixer)] {
                let mut w = workspace();
                w.pinned_transport = true;
                w.select_mode(mode, Vec2::new(width, height));
                let (library, _) = w.dock.find_main_surface_tab(&Pane::Library).unwrap();
                let before_library = serde_json::to_value(&w.dock.main_surface()[library]).unwrap();
                let before_root =
                    serde_json::to_value(&w.dock.main_surface()[NodeIndex::root()]).unwrap();
                let (leaf, _) = w.dock.find_main_surface_tab(&primary).unwrap();
                let region = w.central_region(leaf);
                assert!(!descendant(library, region));
                w.open(Pane::Inspector);
                let (s, inspector, _) = w.dock.find_tab(&Pane::Inspector).unwrap();
                assert_eq!(s, SurfaceIndex::main());
                assert_eq!(inspector.parent(), Some(region));
                assert_eq!(inspector, region.right());
                let egui_dock::Node::Horizontal { fraction, .. } = w.dock.main_surface()[region]
                else {
                    panic!("Inspector requires a right split");
                };
                assert!(((width - library_width) * (1.0 - fraction)) >= 270.0);
                assert!(((width - library_width) * fraction) >= 570.0);
                let (primary_leaf, _) = w.dock.find_main_surface_tab(&primary).unwrap();
                assert!(descendant(primary_leaf, region.left()));
                if mode == 1 {
                    let (notes, _) = w.dock.find_main_surface_tab(&Pane::Notes).unwrap();
                    assert!(descendant(notes, region.left()));
                    assert_ne!(notes, primary_leaf);
                    assert!(matches!(
                        w.dock.main_surface()[region.left()],
                        egui_dock::Node::Vertical { .. }
                    ));
                }
                assert_eq!(
                    before_library,
                    serde_json::to_value(&w.dock.main_surface()[library]).unwrap()
                );
                assert_eq!(
                    before_root,
                    serde_json::to_value(&w.dock.main_surface()[NodeIndex::root()]).unwrap()
                );
                let after = serde_json::to_value(&w.dock).unwrap();
                w.open(Pane::Inspector);
                unique(&w, Pane::Inspector);
                assert_eq!(after, serde_json::to_value(&w.dock).unwrap());
            }
        }
    }
    #[test]
    fn r1_removed_primary_and_notes_reopen_outside_library_in_every_preset() {
        for size in [Vec2::new(1100.0, 700.0), Vec2::new(1440.0, 900.0)] {
            for (mode, primary) in [(1, Pane::Sequencer), (2, Pane::Playlist), (3, Pane::Mixer)] {
                for pane in [primary, Pane::Notes] {
                    let mut w = workspace();
                    w.pinned_transport = true;
                    w.select_mode(mode, size);
                    if let Some(loc) = w.dock.find_tab(&pane) {
                        w.dock.remove_tab(loc);
                    }
                    // Library can become the root when the last editor closes.
                    w.open(pane);
                    w.open(pane);
                    unique(&w, pane);
                    let (s, n, _) = w.dock.find_tab(&pane).unwrap();
                    let (ls, ln, _) = w.dock.find_tab(&Pane::Library).unwrap();
                    assert_eq!(s, SurfaceIndex::main());
                    assert_eq!(ls, s);
                    assert_ne!(ln, n);
                    assert_eq!(
                        w.dock[s][ln].tabs().unwrap(),
                        &[Pane::Library, Pane::Browser]
                    );
                }
                let mut w = workspace();
                w.pinned_transport = true;
                w.select_mode(mode, size);
                for pane in [primary, Pane::Notes] {
                    if let Some(loc) = w.dock.find_tab(&pane) {
                        w.dock.remove_tab(loc);
                    }
                }
                w.open(Pane::Inspector);
                w.open(primary);
                assert_ne!(
                    w.dock.find_tab(&primary).unwrap().1,
                    w.dock.find_tab(&Pane::Library).unwrap().1
                );
                unique(&w, Pane::Inspector);
            }
        }
    }
    #[test]
    fn r1_mode_primary_wins_over_node_order_and_floating_focus() {
        let mut w = workspace();
        w.pinned_transport = true;
        w.mode = 2;
        w.dock = DockState::new(vec![Pane::Mixer]);
        w.dock
            .main_surface_mut()
            .split_right(NodeIndex::root(), 0.5, vec![Pane::Playlist]);
        let float = w.dock.add_window(vec![Pane::Notes, Pane::Analysis]);
        w.dock
            .set_focused_node_and_surface((float, NodeIndex::root()));
        let before = serde_json::to_value(&w.dock[float]).unwrap();
        w.open(Pane::Automation);
        assert_eq!(
            w.dock.find_tab(&Pane::Automation).unwrap().1,
            w.dock.find_tab(&Pane::Playlist).unwrap().1
        );
        assert_ne!(
            w.dock.find_tab(&Pane::Automation).unwrap().1,
            w.dock.find_tab(&Pane::Mixer).unwrap().1
        );
        w.open(Pane::Inspector);
        assert_eq!(
            w.dock.find_tab(&Pane::Inspector).unwrap().0,
            SurfaceIndex::main()
        );
        assert_eq!(before, serde_json::to_value(&w.dock[float]).unwrap());
    }
    #[test]
    fn r1_compact_and_emptied_main_create_safe_central_targets() {
        let mut w = workspace();
        w.pinned_transport = true;
        w.select_mode(1, Vec2::new(800.0, 600.0));
        w.open(Pane::Inspector);
        assert_eq!(
            w.dock.find_tab(&Pane::Inspector).unwrap().1,
            NodeIndex::root().right()
        );
        w.open(Pane::Notes);
        assert_eq!(
            w.dock.find_tab(&Pane::Notes).unwrap().1,
            w.dock.find_tab(&Pane::Sequencer).unwrap().1
        );
        for initially_empty_leaf in [false, true] {
            w.dock = DockState::new(vec![]);
            if !initially_empty_leaf {
                w.dock
                    .main_surface_mut()
                    .push_to_first_leaf(Pane::Sequencer);
                w.dock
                    .remove_tab(w.dock.find_tab(&Pane::Sequencer).unwrap());
                assert!(w.dock.main_surface().is_empty());
            }
            let floating = w.dock.add_window(vec![Pane::Browser]);
            w.dock
                .set_focused_node_and_surface((floating, NodeIndex::root()));
            let before = serde_json::to_value(&w.dock[floating]).unwrap();
            w.open(Pane::Inspector);
            w.open(Pane::Sequencer);
            w.open(Pane::Inspector);
            assert_eq!(
                w.dock.find_tab(&Pane::Inspector).unwrap().0,
                SurfaceIndex::main()
            );
            assert_eq!(
                w.dock.find_tab(&Pane::Sequencer).unwrap().0,
                SurfaceIndex::main()
            );
            unique(&w, Pane::Inspector);
            assert_eq!(before, serde_json::to_value(&w.dock[floating]).unwrap());
        }
        let mut empty = serde_json::to_value(&w.dock).unwrap();
        empty["surfaces"] = json!([]);
        empty["focused_surface"] = Value::Null;
        w.dock = serde_json::from_value(empty).unwrap();
        assert_eq!(w.dock.surfaces_count(), 0);
        w.open(Pane::Inspector);
        assert_eq!(w.dock.surfaces_count(), 1);
        unique(&w, Pane::Inspector);
    }
    #[test]
    fn r1_custom_utility_sibling_limits_branch_and_preserves_user_state() {
        let mut w = workspace();
        w.pinned_transport = true;
        w.mode = 1;
        w.dock = DockState::new(vec![Pane::Sequencer]);
        let [editor, utility] =
            w.dock
                .main_surface_mut()
                .split_below(NodeIndex::root(), 0.6, vec![Pane::Console]);
        let root = serde_json::to_value(&w.dock.main_surface()[NodeIndex::root()]).unwrap();
        let console = serde_json::to_value(&w.dock.main_surface()[utility]).unwrap();
        assert_eq!(w.central_region(editor), editor);
        w.open(Pane::Inspector);
        assert_eq!(
            w.dock.find_tab(&Pane::Inspector).unwrap().1.parent(),
            Some(editor)
        );
        assert_eq!(
            root,
            serde_json::to_value(&w.dock.main_surface()[NodeIndex::root()]).unwrap()
        );
        assert_eq!(
            console,
            serde_json::to_value(&w.dock.main_surface()[utility]).unwrap()
        );
    }
    #[test]
    fn r1_library_only_main_does_not_split_floating_secondary_or_duplicate_inspector() {
        let mut w = workspace();
        w.pinned_transport = true;
        w.mode = 2;
        w.dock = DockState::new(vec![Pane::Library, Pane::Browser]);
        let float = w.dock.add_window(vec![Pane::Notes]);
        w.dock
            .set_focused_node_and_surface((float, NodeIndex::root()));
        let before = serde_json::to_value(&w.dock[float]).unwrap();
        w.open(Pane::Playlist);
        let (library, _) = w.dock.find_main_surface_tab(&Pane::Library).unwrap();
        let (playlist, _) = w.dock.find_main_surface_tab(&Pane::Playlist).unwrap();
        assert_eq!(library, NodeIndex::root().left());
        assert_eq!(playlist, NodeIndex::root().right());
        assert!(
            matches!(w.dock.main_surface()[NodeIndex::root()], egui_dock::Node::Horizontal { fraction, .. } if (fraction - 0.22).abs() < 0.001)
        );
        w.open(Pane::Inspector);
        assert_eq!(before, serde_json::to_value(&w.dock[float]).unwrap());
        let loc = w.dock.find_tab(&Pane::Inspector).unwrap();
        w.dock.remove_tab(loc);
        let inspector_float = w.dock.add_window(vec![Pane::Inspector]);
        let before: Vec<_> = w
            .dock
            .iter_all_nodes()
            .map(|(s, n)| (s.0, serde_json::to_value(n).unwrap()))
            .collect();
        w.open(Pane::Inspector);
        w.open(Pane::Inspector);
        unique(&w, Pane::Inspector);
        assert_eq!(
            w.dock.find_tab(&Pane::Inspector).unwrap().0,
            inspector_float
        );
        // Focus changes are permitted; no pane or tree geometry may change.
        let after: Vec<_> = w
            .dock
            .iter_all_nodes()
            .map(|(s, n)| (s.0, serde_json::to_value(n).unwrap()))
            .collect();
        assert_eq!(before, after);
    }
    #[test]
    fn r1_custom_mixed_library_leaf_is_not_a_central_anchor() {
        let mut w = workspace();
        w.pinned_transport = true;
        w.mode = 2;
        w.dock = DockState::new(vec![Pane::Library, Pane::Browser, Pane::Playlist]);
        w.open(Pane::Notes);
        assert_ne!(
            w.dock.find_tab(&Pane::Notes).unwrap().1,
            w.dock.find_tab(&Pane::Library).unwrap().1
        );
        assert_eq!(w.dock.iter_all_tabs().count(), 4);
        w.open(Pane::Inspector);
        unique(&w, Pane::Playlist);
    }
    #[test]
    fn r1_custom_focus_and_last_utility_leaf_are_reused() {
        let mut w = workspace();
        w.pinned_transport = true;
        w.mode = 0;
        w.dock = DockState::new(vec![Pane::Sequencer]);
        let [_, notes] =
            w.dock
                .main_surface_mut()
                .split_below(NodeIndex::root(), 0.5, vec![Pane::Notes]);
        w.dock
            .set_focused_node_and_surface((SurfaceIndex::main(), notes));
        w.open(Pane::Vocal);
        assert_eq!(w.dock.find_tab(&Pane::Vocal).unwrap().1, notes);
        w.dock = DockState::new(vec![Pane::Console]);
        w.open(Pane::Playlist);
        assert_eq!(
            w.dock.find_tab(&Pane::Playlist).unwrap().1,
            NodeIndex::root()
        );
        assert_eq!(
            w.dock.find_tab(&Pane::Console).unwrap().1,
            NodeIndex::root()
        );
    }
    #[test]
    fn r1_legacy_custom_floating_open_policy_is_unchanged() {
        let mut w = workspace();
        w.dock = DockState::new(vec![Pane::Browser]);
        let float = w.dock.add_window(vec![Pane::Playlist]);
        w.dock
            .set_focused_node_and_surface((float, NodeIndex::root()));
        w.open(Pane::Inspector);
        // This is intentionally the old policy, not the pinned-shell policy.
        assert_eq!(
            w.dock.find_tab(&Pane::Inspector).unwrap().0,
            SurfaceIndex::main()
        );
        assert_eq!(
            w.dock.find_tab(&Pane::Inspector).unwrap().1,
            w.dock.find_tab(&Pane::Browser).unwrap().1
        );
        w.open(Pane::Inspector);
        unique(&w, Pane::Inspector);
        assert!(w.dock[float][NodeIndex::root()]
            .tabs()
            .unwrap()
            .contains(&Pane::Playlist));
    }

    #[test]
    fn studio_regions_small_and_uninitialized() {
        for (w, h) in [
            (800.0, 600.0),
            (0.0, 0.0),
            (f32::NAN, 700.0),
            (1100.0, f32::INFINITY),
        ] {
            assert_eq!(studio_regions(w, h, true), (None, None));
        }
    }
    #[test]
    fn studio_regions_desktop_sizes() {
        for (w, h, l, b) in [(1100.0, 700.0, 216.0, 240.0), (1440.0, 900.0, 240.0, 310.0)] {
            let (left, top) = studio_regions(w, h, true);
            assert!((left.unwrap() * w - l).abs() < 0.01);
            assert!(((1.0 - top.unwrap()) * (h - 88.0) - b).abs() < 0.01);
            assert_eq!(studio_regions(w, h, false).1, None);
        }
    }
    #[test]
    fn studio_regions_boundaries() {
        assert_eq!(studio_regions(999.0, 700.0, true), (None, None));
        assert!(studio_regions(1000.0, 700.0, true).0.is_some());
        assert_eq!(studio_regions(1100.0, 649.0, true).1, None);
        assert!(studio_regions(1100.0, 650.0, true).1.is_some());
    }
    #[test]
    fn studio_regions_keep_fraction_bounds() {
        for w in [1000.0, 1100.0, 1299.0, 1300.0, 1440.0, 3840.0] {
            for h in [1.0, 600.0, 650.0, 700.0, 799.0, 800.0, 900.0, 2160.0] {
                let (left, top) = studio_regions(w, h, true);
                for f in [left, top].into_iter().flatten() {
                    assert!(f.is_finite() && f > 0.0 && f < 1.0);
                }
            }
        }
    }

    #[test]
    fn captures_use_requested_panel_without_layout_persistence() {
        let ctx = egui::Context::default();
        for view in 0..7 {
            let mut capture = Workspace::capture(&ctx, view);
            assert!(capture.dock.find_tab(&Pane::ALL[view]).is_some());
            assert!(capture.pinned_transport);
            assert!(capture.dock.find_tab(&Pane::Transport).is_none());
            assert!(capture.path.as_os_str().is_empty());
            assert_eq!(capture.mode, 0);
            capture.flush().unwrap();
            capture.persist(1.0);
            capture.persist(2.0);
            assert_eq!(capture.changed_at, 0.0);
        }
    }
    #[test]
    fn graphite_presets_keep_real_editors_and_no_duplicate_transport() {
        for (width, height) in [(1100.0, 700.0), (1440.0, 900.0)] {
            let mut w = workspace();
            w.pinned_transport = true;
            for (mode, pane) in [(1, Pane::Sequencer), (2, Pane::Playlist), (3, Pane::Mixer)] {
                w.select_mode(mode, Vec2::new(width, height));
                assert_eq!(w.mode, mode);
                assert!(w.dock.find_tab(&pane).is_some());
                assert!(w.dock.find_tab(&Pane::Library).is_some());
                assert!(w.dock.find_tab(&Pane::Browser).is_some());
                assert!(w.dock.find_tab(&Pane::Transport).is_none());
                let egui_dock::Node::Horizontal { fraction, .. } =
                    w.dock.main_surface()[NodeIndex::root()]
                else {
                    panic!("Library must be a column")
                };
                let library_pixels = if width < 1300.0 { 216.0 } else { 240.0 };
                assert!((fraction * width - library_pixels).abs() < 0.01);
                assert!(w.dock.main_surface()[NodeIndex(1)]
                    .tabs()
                    .unwrap()
                    .contains(&Pane::Library));
                assert_eq!(w.dock.find_tab(&Pane::Notes).is_some(), mode == 1);
                let tabs: Vec<_> = w.dock.iter_all_tabs().map(|(_, p)| *p).collect();
                let unique: std::collections::HashSet<_> = tabs.iter().collect();
                assert_eq!(tabs.len(), unique.len());
            }
        }
    }
    #[test]
    fn compact_preset_has_one_editor_and_all_panels_can_reopen() {
        let mut w = workspace();
        w.pinned_transport = true;
        w.select_mode(1, Vec2::new(800.0, 600.0));
        assert_eq!(w.dock.iter_all_tabs().count(), 1);
        for pane in Pane::ALL {
            w.open(pane);
            assert!(w.dock.find_tab(&pane).is_some());
        }
    }
    #[test]
    fn legacy_preferences_default_to_docked_transport_and_roundtrip_exactly() {
        let w = workspace();
        // Use the real save path to normalize unrendered dock geometry to finite values.
        w.flush().unwrap();
        let mut old: Value = serde_json::from_slice(&std::fs::read(&w.path).unwrap()).unwrap();
        std::fs::remove_dir_all(w.path.parent().unwrap()).unwrap();
        let fields = old.as_object_mut().unwrap();
        fields.remove("pinned_transport");
        fields.remove("mode");
        let saved: Saved = serde_json::from_value(old.clone()).unwrap();
        assert!(!saved.pinned_transport);
        assert_eq!(saved.mode, 0);
        assert_eq!(serde_json::to_value(&saved.dock).unwrap(), old["dock"]);
        let new = Saved {
            version: 1,
            dock: saved.dock,
            pinned_transport: true,
            mode: 2,
        };
        let roundtrip: Saved = serde_json::from_str(&serde_json::to_string(&new).unwrap()).unwrap();
        assert!(roundtrip.pinned_transport);
        assert_eq!(roundtrip.mode, 2);
    }
    #[test]
    fn standard_transport_keeps_152_points_after_resize() {
        let mut d = Workspace::default_dock(3, 860.0);
        Workspace::keep_transport_visible(&mut d, 600.0);
        let egui_dock::Node::Vertical { fraction, .. } = d.main_surface()[NodeIndex::root()] else {
            panic!()
        };
        assert!((fraction * 600.0 - 152.0).abs() < 0.01);
        Workspace::keep_transport_visible(&mut d, 800.0);
        let egui_dock::Node::Vertical {
            fraction: larger, ..
        } = d.main_surface()[NodeIndex::root()]
        else {
            panic!()
        };
        assert_eq!(fraction, larger); // never shrink an existing strip
    }
    #[test]
    fn custom_and_tabbed_top_docks_are_untouched() {
        let mut d = Workspace::default_dock(3, 860.0);
        d.main_surface_mut()[NodeIndex(1)].append_tab(Pane::Mixer);
        let before = format!("{d:?}");
        Workspace::keep_transport_visible(&mut d, 400.0);
        assert_eq!(format!("{d:?}"), before);
        let mut d = DockState::new(vec![Pane::Transport]);
        Workspace::keep_transport_visible(&mut d, 400.0);
        assert_eq!(d.main_surface().len(), 1);
    }

    use super::*;
    fn workspace() -> Workspace {
        Workspace {
            dock: Workspace::default_dock(3, 700.0),
            saved: String::new(),
            changed_at: 0.0,
            path: std::env::temp_dir()
                .join(format!(
                    "beatbox_layout_test_{}_{}",
                    std::process::id(),
                    std::thread::current().name().unwrap_or("unnamed")
                ))
                .join("layout.json"),
            requested: None,
            fresh: false,
            pinned_transport: false,
            mode: 0,
            reset: false,
        }
    }
    #[test]
    fn the_browser_opens_beside_the_editor_not_on_top_of_it() {
        let mut w = workspace();
        let editor = w.dock.find_tab(&Pane::Playlist).unwrap();
        w.open(Pane::Browser);
        let browser = w.dock.find_tab(&Pane::Browser).unwrap();
        // same window, its own column: the editor is still visible and still where it was
        assert_eq!(browser.0, editor.0);
        assert_ne!(browser.1, editor.1);
        let editor_after = w.dock.find_tab(&Pane::Playlist).unwrap();
        assert_ne!(browser.1, editor_after.1);
        let transport = w.dock.find_tab(&Pane::Transport).unwrap();
        assert_ne!(browser.1, transport.1);
        // opening it again only focuses it
        w.open(Pane::Browser);
        assert_eq!(
            w.dock
                .iter_all_tabs()
                .filter(|(_, x)| **x == Pane::Browser)
                .count(),
            1
        );
    }
    #[test]
    fn every_panel_can_hide_reopen_and_focus_without_duplicates() {
        let mut w = workspace();
        for p in Pane::ALL {
            w.open(p);
            w.open(p);
            assert_eq!(w.dock.iter_all_tabs().filter(|(_, x)| **x == p).count(), 1);
            let loc = w.dock.find_tab(&p).unwrap();
            w.dock.remove_tab(loc);
            assert!(w.dock.find_tab(&p).is_none());
            w.open(p);
            assert!(w.dock.find_tab(&p).is_some());
        }
    }
    #[test]
    fn default_is_quiet_and_corrupt_or_old_layout_recovers() {
        for text in [
            None,
            Some("broken"),
            Some(r#"{"version":999,"dock":{"surfaces":[],"focused_surface":0}}"#),
        ] {
            let d = Workspace::restore(
                text,
                3,
                Rect::from_min_size(Pos2::ZERO, Vec2::new(1100.0, 700.0)),
            );
            assert_eq!(d.iter_all_tabs().count(), 2);
            assert!(d.find_tab(&Pane::Playlist).is_some());
            assert!(d.find_tab(&Pane::Transport).is_some());
        }
    }
    #[test]
    fn saved_floating_editor_reopens_expanded_and_within_display() {
        let mut d = DockState::new(vec![Pane::Transport]);
        let si = d.add_window(vec![Pane::Notes]);
        d[si][NodeIndex::root()].set_rect(Rect::from_min_size(
            Pos2::new(2500.0, 1600.0),
            Vec2::new(900.0, 500.0),
        ));
        let mut w = workspace();
        w.path = w.path.with_file_name("recovery.json");
        w.dock = d;
        w.flush().unwrap();
        let text = std::fs::read_to_string(&w.path).unwrap();
        let _ = std::fs::remove_file(&w.path);
        let bounds = Rect::from_min_max(Pos2::new(12.0, 12.0), Pos2::new(1088.0, 688.0));
        let restored = Workspace::restore(Some(&text), 3, bounds);
        let v = serde_json::to_value(restored).unwrap();
        let w = &v["surfaces"][1]["Window"][1];
        assert_eq!(w["new"], json!(true));
        let p = &w["next_position"];
        let z = &w["next_size"];
        let x = p["x"].as_f64().unwrap();
        let y = p["y"].as_f64().unwrap();
        assert!(x >= 12.0 && y >= 60.0);
        assert!(x + z["x"].as_f64().unwrap() <= 1088.0);
        assert!(y + z["y"].as_f64().unwrap() <= 688.0);
    }
    #[test]
    fn closed_floating_surface_does_not_break_next_start() {
        let mut w = workspace();
        let closed = w.dock.add_window(vec![Pane::Analysis]);
        w.dock.add_window(vec![Pane::History]);
        w.dock.remove_surface(closed);
        w.flush().unwrap();
        let text = std::fs::read_to_string(&w.path).unwrap();
        let _ = std::fs::remove_file(&w.path);
        let d = Workspace::restore(
            Some(&text),
            3,
            Rect::from_min_size(Pos2::ZERO, Vec2::new(1100.0, 700.0)),
        );
        assert!(d.find_tab(&Pane::History).is_some());
        assert!(d.find_tab(&Pane::Analysis).is_none());
    }
    #[test]
    fn split_and_floating_layout_round_trip_separately_from_project() {
        let mut w = workspace();
        for p in Pane::ALL[..7].iter() {
            w.open(*p);
        }
        let loc = w.dock.find_tab(&Pane::Playlist).unwrap();
        w.dock.remove_tab(loc);
        w.dock.add_window(vec![Pane::Playlist]);
        w.dock
            .main_surface_mut()
            .split_right(NodeIndex(2), 0.72, vec![Pane::Notes]);
        w.flush().unwrap();
        let saved: Saved = serde_json::from_slice(&std::fs::read(&w.path).unwrap()).unwrap();
        assert_eq!(saved.version, 1);
        assert_eq!(saved.dock.surfaces_count(), 2);
        for p in Pane::ALL[..7].iter().chain([Pane::Notes].iter()) {
            assert!(saved.dock.find_tab(p).is_some());
        }
        assert_eq!(
            saved
                .dock
                .iter_all_tabs()
                .map(|(_, p)| *p)
                .collect::<Vec<_>>(),
            w.dock.iter_all_tabs().map(|(_, p)| *p).collect::<Vec<_>>()
        );
        let _ = std::fs::remove_dir_all(w.path.parent().unwrap());
    }
}
