//! Movable studio panels. Layout data is separate from the musical project.
use super::*;
use egui_dock::{DockArea, DockState, NodeIndex, Style, TabViewer};
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
    fn view(self) -> Option<usize> {
        Self::ALL[..7].iter().position(|p| *p == self)
    }
}
#[derive(Serialize, Deserialize)]
struct Saved {
    version: u32,
    dock: DockState<Pane>,
}
pub struct Workspace {
    dock: DockState<Pane>,
    saved: String,
    changed_at: f64,
    path: PathBuf,
    requested: Option<Pane>,
    /// No saved layout yet: rebuild the default once the real window size is known.
    fresh: bool,
    /// Last workflow tab picked in the header: 0 none, 1 Beat, 2 Arrange, 3 Mix.
    mode: u8,
    /// Ctrl+Shift+H: put the windows back to the default arrangement.
    reset: bool,
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
    /// Automated captures never restore or save a person's interactive layout.
    pub fn capture(ctx: &egui::Context, view: usize) -> Self {
        Self {
            dock: Self::default_dock(view, ctx.screen_rect().height()),
            saved: String::new(),
            changed_at: 0.0,
            path: PathBuf::new(),
            requested: None,
            fresh: true,
            mode: 0,
            reset: false,
        }
    }
    pub fn load(ctx: &egui::Context, view: usize) -> Self {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_else(std::env::temp_dir);
        let path = base.join("beatbox/studio-layout-v1.json");
        let text = std::fs::read_to_string(&path).ok();
        let fresh = text.is_none();
        let dock = Self::restore(text.as_deref(), view, ctx.screen_rect().shrink(12.0));
        Self {
            dock,
            saved: String::new(),
            changed_at: 0.0,
            path,
            requested: None,
            fresh,
            mode: if fresh && view == 3 { 2 } else { 0 },
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
    fn open(&mut self, p: Pane) {
        if let Some((s, n, t)) = self.dock.find_tab(&p) {
            self.dock[s].set_active_tab(n, t);
            self.dock.set_focused_node_and_surface((s, n));
        } else {
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
    generation: Option<u64>,
    log: &'a (Vec<String>, Vec<String>),
    undo: (usize, usize),
    request: Option<Pane>,
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
        if self.studio.feel_open() {
            ui.disable();
        }
        ui.set_min_size(match pane {
            Pane::Transport => Vec2::new(420.0, 78.0),
            Pane::Console => Vec2::new(420.0, 230.0),
            Pane::Inspector | Pane::Library | Pane::Browser | Pane::Analysis | Pane::History => {
                Vec2::new(250.0, 220.0)
            }
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
            Pane::Transport => {
                egui::ScrollArea::both()
                    .id_salt("transport")
                    .auto_shrink([false, false])
                    .show(ui, |ui| s.top_bar(ui, p, self.undo));
            }
            Pane::Library => s.browser(ui, p),
            Pane::Inspector => s.inspector(ui, p, self.rev, self.log),
            Pane::Console => s.console_dock(ui, p),
            Pane::Notes => s.piano_roll(ui, p, self.rev, self.generation),
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
                    && !matches!(pane, Pane::Browser | Pane::Automation | Pane::Vocal)
                {
                    ui.add_space(28.0);
                    ui.label(
                        RichText::new("Make room for your next idea.")
                            .size(28.0)
                            .strong(),
                    );
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new("Start with a sound. Shape the workspace as you go.")
                            .color(DIM),
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
                    ui.add_space(28.0);
                }
                match pane {
                    Pane::Sequencer => {
                        s.arrangement(ui, p);
                        ui.add_space(12.0);
                        s.sequencer(ui, p);
                    }
                    Pane::Mixer => s.mixer(ui, p),
                    Pane::Rack => s.rack_view(ui, p, self.rev),
                    Pane::Playlist => s.playlist_view(ui, p, self.rev),
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
        // seen_project_load was captured before this Project snapshot. Pair it
        // only with a still-current frame revision under lock, never a newer load.
        let generation = {
            let e = self.engine.lock().unwrap();
            super::piano::feel_frame_generation(&e, rev, self.seen_project_load)
        };
        let mut ws = std::mem::replace(
            &mut self.workspace,
            Workspace {
                dock: DockState::new(vec![]),
                saved: String::new(),
                changed_at: 0.0,
                path: PathBuf::new(),
                requested: None,
                fresh: false,
                mode: 0,
                reset: false,
            },
        );
        let bounds = ctx.screen_rect().shrink(12.0);
        if std::mem::take(&mut ws.fresh) {
            ws.dock = Workspace::default_dock(self.view, ctx.screen_rect().height());
        }
        if std::mem::take(&mut ws.reset) {
            ws.mode = 0;
            ws.dock = Workspace::default_dock(self.view, ctx.screen_rect().height());
        }
        if let Some(p) = ws.requested.take() {
            ws.open(p);
        }
        egui::TopBottomPanel::top("workspace_header")
            .exact_height(54.0)
            .frame(
                egui::Frame::none()
                    .fill(BG)
                    .inner_margin(egui::Margin::symmetric(18.0, 10.0)),
            )
            .show(ctx, |ui| {
                if self.feel_open() { ui.disable(); }
                ui.horizontal(|ui| {
                    if self.brand.is_none() {
                        if let Ok(im) = image::load_from_memory(include_bytes!(
                            "../../assets/reel/logo/mark.png"
                        )) {
                            let im = im.to_rgba8();
                            self.brand = Some(ctx.load_texture(
                                "wound",
                                egui::ColorImage::from_rgba_unmultiplied(
                                    [im.width() as usize, im.height() as usize],
                                    im.as_raw(),
                                ),
                                egui::TextureOptions::LINEAR,
                            ));
                        }
                    }
                    if let Some(mark) = &self.brand {
                        ui.image((mark.id(), Vec2::splat(28.0)));
                    }
                    ui.label(
                        RichText::new("beatbox")
                            .font(FontId::new(19.0, egui::FontFamily::Name("brand".into()))),
                    );
                    ui.label(RichText::new("rs").color(ACCENT));
                    ui.add_space(20.0);
                    let h = ui.ctx().screen_rect().height();
                    for (m, name, tip) in [
                        (
                            1u8,
                            "Beat",
                            "Write drums and melodies: step sequencer and piano roll",
                        ),
                        (2u8, "Arrange", "Lay patterns out into a song"),
                        (3u8, "Mix", "Balance levels, effects and the master"),
                    ] {
                        let on = ws.mode == m;
                        let label = RichText::new(name).size(14.0).color(if on {
                            egui::Color32::from_rgb(244, 232, 214)
                        } else {
                            DIM
                        });
                        let mut btn = egui::Button::new(label);
                        btn = if on {
                            btn.fill(egui::Color32::from_rgb(74, 30, 24))
                                .stroke(egui::Stroke::new(1.0_f32, ACCENT))
                        } else {
                            btn.frame(false)
                        };
                        if ui.add(btn).on_hover_text(tip).clicked() {
                            ws.mode = m;
                            ws.dock = match m {
                                1 => Workspace::beat_dock(h),
                                2 => Workspace::arrange_dock(h),
                                _ => Workspace::mix_dock(h),
                            };
                        }
                    }
                    ui.add_space(14.0);
                    ui.menu_button("Panels", |ui| {
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
                                if !main.contains(&pane) {
                                    toggle(ui, pane);
                                }
                            }
                        });
                    });
                    ui.menu_button("Layout", |ui| {
                        if ui.button("Reset workspace").clicked() {
                            ws.mode = 0;
                            ws.dock = Workspace::default_dock(self.view, h);
                            ui.close_menu();
                        }
                        if ui.button("Focus on one panel").clicked() {
                            ws.mode = 0;
                            ws.dock = DockState::new(vec![Pane::ALL[self.view.min(6)]]);
                            ui.close_menu();
                        }
                        ui.separator();
                        ui.label(
                            RichText::new("Drag tabs to split, group or float panels.").size(11.0),
                        );
                        ui.label(
                            RichText::new("Drag dividers to resize. Layout saves automatically.")
                                .size(11.0),
                        );
                    });
                    ui.menu_button("Help", |ui| {
                        ui.set_max_width(420.0);
                        ui.heading("Studio guide");
                        ui.label("Beat: write drums in the channel rack and melodies in the piano roll.");
                        ui.label("Arrange: place patterns and edit clips on the song timeline.");
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
                        ui.label(RichText::new(state).color(if state == "Saved" { DIM } else { ACCENT }).size(11.0))
                            .on_hover_text(self.save_tooltip());
                        ui.add(egui::Label::new(RichText::new(&p.name).color(DIM).size(12.0)).truncate())
                            .on_hover_text(&p.name);
                        if self.rendering.is_some() && ui.ctx().screen_rect().width() >= 900.0 {
                            ui.spinner();
                        }
                    });
                });
            });
        let active_before = self.view;
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(BG)
                    .inner_margin(egui::Margin::symmetric(14.0, 8.0)),
            )
            .show(ctx, |ui| {
                Workspace::keep_transport_visible(&mut ws.dock, ui.available_height());
                let mut style = Style::from_egui(ui.style());
                style.tab_bar.height = 30.0;
                style.separator.extra = 48.0;
                style.tab_bar.bg_fill = BG;
                style.tab_bar.hline_color = LINE;
                style.tab.tab_body.inner_margin = egui::Margin::same(16.0);
                style.tab.tab_body.bg_fill = BG;
                style.separator.width = 6.0;
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
                    generation,
                    log,
                    undo,
                    request: None,
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
        ws.persist(ctx.input(|i| i.time));
        self.workspace = ws;
        self.feel_ui(ctx, p, rev, generation);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn captures_use_requested_panel_without_layout_persistence() {
        let ctx = egui::Context::default();
        for view in 0..7 {
            let mut capture = Workspace::capture(&ctx, view);
            assert!(capture.dock.find_tab(&Pane::ALL[view]).is_some());
            assert!(capture.dock.find_tab(&Pane::Transport).is_some());
            assert_eq!(capture.dock.iter_all_tabs().count(), 2);
            assert!(capture.path.as_os_str().is_empty());
            assert_eq!(capture.mode, 0);
            capture.flush().unwrap();
            capture.persist(1.0);
            capture.persist(2.0);
            assert_eq!(capture.changed_at, 0.0);
        }
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
