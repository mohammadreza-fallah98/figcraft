//! Log in to a company portal from the right panel's top bar, next to Share (desktop app, and only
//! when a portal address is configured: `FIGCRAFT_PORTAL_URL`, see `figcraft-portal`). The button
//! opens a popover: Log in (the portal's page opens in the browser and `figcraft://auth` brings the
//! one-time code back), then the person's projects, each opened with the join link the portal
//! gives. The name from the login goes into Preferences ▸ Your name. All the work is
//! `figcraft_portal::Portal`'s, on background threads; this file only draws it and applies its
//! results (a name to keep, a project to join) once a frame.
//!
//! Signed in, there is no button of its own: the avatar's people menu (`live::people_menu`) is the
//! profile menu. It calls `profile_top` (name, company, projects, Refresh) before the people and
//! `profile_footer` (Log out, last row) after them.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use egui::{CornerRadius, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use figcraft_portal::config::ConfigError;
use figcraft_portal::session::Projects;
use figcraft_portal::{Access, Portal, Status, View};
use serde_json::json;

use crate::FigApp;
use crate::a11y::Tip;
use crate::i18n::tr;
use crate::right::{menu_item, menu_popup};
use crate::theme::{Tokens, semibold};
use crate::widgets;

/// The portal's place in the UI state.
#[derive(Default)]
pub struct PortalUi {
    /// Made the first time it is needed: the portal, or why there is none.
    portal: Option<Result<Portal, ConfigError>>,
    /// The window's context, for waking it from the background threads.
    ctx: Arc<Mutex<Option<egui::Context>>>,
    /// The settings folder: where a login leaves its note for a second copy of the app.
    state_dir: Option<PathBuf>,
}

/// Where the app keeps its settings, for the note a login leaves for another copy of the app
/// (the system starts a new process for `figcraft://auth`). Call before the first frame.
pub fn set_state_dir(app: &mut FigApp, dir: Option<PathBuf>) {
    app.ui.portal.state_dir = dir;
}

impl PortalUi {
    #[cfg(not(target_arch = "wasm32"))]
    fn get(&mut self) -> &Result<Portal, ConfigError> {
        let (ctx, dir) = (self.ctx.clone(), self.state_dir.clone());
        self.portal.get_or_insert_with(|| {
            let wake = Box::new(move || {
                if let Some(c) = ctx.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
                    c.request_repaint();
                }
            });
            Portal::system(wake, dir)
        })
    }

    /// The browser build has no way to get the code back: no portal there.
    #[cfg(target_arch = "wasm32")]
    fn get(&mut self) -> &Result<Portal, ConfigError> {
        self.portal.get_or_insert(Err(ConfigError::NotSet))
    }
}

#[cfg(test)]
impl PortalUi {
    /// A portal made by a test.
    pub(crate) fn install(&mut self, portal: Result<Portal, ConfigError>) {
        self.portal = Some(portal);
    }
}

/// A `figcraft:` link the app was handed: `figcraft://auth?code=…` is the portal's login coming
/// back (true); any other link is not this module's.
pub fn handle_link(app: &mut FigApp, link: &str) -> bool {
    if !figcraft_portal::is_auth_link(link) {
        return false;
    }
    if let Ok(portal) = app.ui.portal.get() {
        portal.handle_auth_link(link);
    }
    true
}

/// Left padding of the top bar's people area (the own avatar, or the Log in button in its place).
pub const PEOPLE_PAD: f32 = 12.0;

/// True when a portal is configured and the person is not signed in: the Log in button then stands
/// alone, and `live::people_bar` leaves out the own avatar (it comes back, as the profile menu, once
/// signed in). Without a portal this is false: upstream behaviour.
pub fn login_only(app: &mut FigApp) -> bool {
    match app.ui.portal.get() {
        Err(ConfigError::NotSet) => false,
        Err(_) => true,
        Ok(p) => p.view().status != Status::SignedIn,
    }
}

/// The Log in button, at the left end of the top bar where the own avatar sits once signed in.
/// Returns the x where the people area starts: after the button, or at the bar's padding when
/// nothing was drawn (no portal configured, or signed in: the avatar's menu is the profile menu).
pub fn top_bar(app: &mut FigApp, ui: &mut Ui, top: Rect) -> f32 {
    let start = top.min.x + PEOPLE_PAD;
    app.ui.portal.ctx.lock().unwrap_or_else(PoisonError::into_inner).get_or_insert_with(|| ui.ctx().clone());
    let portal = match app.ui.portal.get().clone() {
        Err(ConfigError::NotSet) => return start,
        other => other,
    };
    let view = portal.as_ref().ok().map(Portal::view);
    if let Ok(p) = &portal {
        p.restore();
        apply_results(app, p);
        keep_awake(ui.ctx(), view.as_ref());
    }
    if view.as_ref().is_some_and(|v| v.status == Status::SignedIn) {
        return start;
    }
    let t = Tokens::get(ui.ctx());
    let galley = ui.painter().layout_no_wrap(tr("Log in").to_string(), semibold(11.0), t.text);
    let width = (galley.size().x + 20.0).clamp(52.0, 120.0);
    let rect = Rect::from_min_size(pos2(start, top.center().y - 14.0), vec2(width, 28.0));
    let resp = ui.interact(rect, ui.scope_id().with("portal_login"), Sense::click()).tip(tr("Log in to your company portal"));
    let r = CornerRadius::same(6);
    if resp.hovered() {
        ui.painter().rect_filled(rect, r, t.hover);
    }
    ui.painter().rect_stroke(rect, r, Stroke::new(1.0, t.border), StrokeKind::Inside);
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, t.text);
    popover(app, &resp, &portal, view.as_ref());
    rect.max.x + 6.0
}

/// The signed-in portal and its view, if the person is signed in.
fn signed_in_view(app: &mut FigApp) -> Option<(Portal, View)> {
    let portal = app.ui.portal.get().clone().ok()?;
    let view = portal.view();
    (view.status == Status::SignedIn).then_some((portal, view))
}

/// Hook for the people menu, drawn after the people rows: when signed in, a separator, then the portal
/// projects (a click opens one) and Refresh; a separator ends it. Draws nothing when signed out.
pub fn profile_top(app: &mut FigApp, ui: &mut Ui) {
    let Some((portal, view)) = signed_in_view(app) else { return };
    let t = Tokens::get(ui.ctx());
    ui.separator();
    ui.label(egui::RichText::new(tr("Projects")).font(semibold(12.0)));
    match &view.projects {
        Projects::NotLoaded | Projects::Loading => widgets::label(ui, tr("Loading projects…"), t.text_secondary),
        Projects::Failed(message) => widgets::label(ui, message, t.danger),
        Projects::Loaded(list) if list.is_empty() => widgets::label(ui, tr("No projects are shared with you yet."), t.text_secondary),
        Projects::Loaded(list) => {
            egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                for p in list {
                    let access = match &p.access {
                        Access::View => tr("View").to_string(),
                        Access::Edit => tr("Edit").to_string(),
                        Access::Other(other) => other.clone(),
                    };
                    if menu_item(ui, &p.name, &access) && view.opening.is_none() {
                        portal.open_project(p.id);
                        ui.close();
                    }
                }
            });
        }
    }
    if view.opening.is_some() {
        widgets::label(ui, tr("Opening the project…"), t.text_secondary);
    }
    if menu_item(ui, tr("Refresh"), "") && !view.busy {
        portal.refresh_projects();
    }
    ui.separator();
}

/// Hook for the people menu, its last row: Log out (and the note when the login can't be kept).
/// Draws nothing when signed out.
pub fn profile_footer(app: &mut FigApp, ui: &mut Ui) {
    let Some((portal, view)) = signed_in_view(app) else { return };
    let t = Tokens::get(ui.ctx());
    ui.separator();
    if menu_item(ui, tr("Log out"), "") {
        portal.logout();
        ui.close();
    }
    if !view.remembered {
        widgets::label(ui, tr("Staying logged in between runs isn't available on this system."), t.text_secondary);
    }
}

/// What the background work finished: the name to keep, a project to join.
fn apply_results(app: &mut FigApp, portal: &Portal) {
    if let Some(name) = portal.take_display_name() {
        app.run("pref.userName", json!({"name": name}));
        app.services.save_prefs(&app.session.prefs);
    }
    if let Some(opened) = portal.take_opened() {
        crate::live::start_join(app, &opened.join_url);
    }
    portal.poll_handoff();
}

/// Frames keep coming while something is going on (a callback from another copy of the app, or a
/// request in flight).
fn keep_awake(ctx: &egui::Context, view: Option<&View>) {
    match view {
        Some(v) if v.status == Status::WaitingForBrowser => ctx.request_repaint_after(Duration::from_secs(1)),
        Some(v) if v.busy => ctx.request_repaint_after(Duration::from_millis(250)),
        _ => {}
    }
}

fn popover(app: &mut FigApp, anchor: &egui::Response, portal: &Result<Portal, ConfigError>, view: Option<&View>) {
    menu_popup(anchor).width(300.0).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        let t = Tokens::get(ui.ctx());
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.set_max_width(300.0);
        ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
            ui.add_space(4.0);
            match (portal, view) {
                (Ok(portal), Some(view)) => body(app, ui, &t, portal, view),
                (Err(e), _) => widgets::label(ui, &e.to_string(), t.danger),
                _ => {}
            }
        });
    });
}

fn body(app: &mut FigApp, ui: &mut Ui, t: &Tokens, portal: &Portal, view: &View) {
    ui.label(egui::RichText::new(tr("Company portal")).font(semibold(12.0)));
    if let Some(notice) = &view.notice {
        widgets::label(ui, notice, t.danger);
    }
    match &view.status {
        Status::SignedIn => ui.close(),
        Status::SigningIn => widgets::label(ui, tr("Signing in…"), t.text_secondary),
        Status::WaitingForBrowser => {
            widgets::label(ui, tr("Finish logging in in your browser. FigCraft continues here when you are done."), t.text_secondary);
            if widgets::button(ui, tr("Cancel"), false, true).clicked() {
                portal.cancel_login();
            }
        }
        Status::SignedOut | Status::Failed(_) => {
            if let Status::Failed(message) = &view.status {
                widgets::label(ui, message, t.danger);
            }
            widgets::label(ui, tr("Log in to your company portal to open the projects shared with you."), t.text_secondary);
            if widgets::button(ui, tr("Log in"), true, true).clicked() {
                start_login(app, portal);
            }
        }
    }
}

/// Open the portal's login page in the browser.
fn start_login(app: &mut FigApp, portal: &Portal) {
    match portal.begin_login() {
        Ok(url) => app.services.open_url(&url),
        Err(e) => app.show_error(e),
    }
}
