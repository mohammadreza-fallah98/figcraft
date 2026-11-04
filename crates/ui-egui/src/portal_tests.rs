//! The portal login in the right panel's top bar: no button without a configured portal, the
//! popover's Log in opens the portal's login page, `figcraft://auth` goes to the portal and every
//! other link to the Join dialog.

use std::sync::{Arc, Mutex};

use egui::Pos2;
use figcraft_portal::api::{Access, Login, OpenedProject, PortalError, ProjectEntry};
use figcraft_portal::config::ConfigError;
use figcraft_portal::secret::MemoryStore;
use figcraft_portal::session::{Api, Portal, Status};

use crate::Services;
use crate::live_tests::{Harness, has_text, text_center, texts};

const BASE: &str = "https://portal.example";

/// A portal that signs anyone in as Ada, with one project whose join link is a local session.
struct Scripted;

impl Api for Scripted {
    fn exchange_code(&self, _: &str, _: &str) -> Result<Login, PortalError> {
        let login = Login {
            username: "ada".into(),
            first_name: Some("Ada".into()),
            last_name: Some("Lovelace".into()),
            token: "AT".into(),
            expire_time: 3600,
            refresh_token: "RT".into(),
            refresh_expire_time: 100_000,
            ..Login::default()
        };
        Ok(login)
    }
    fn refresh(&self, _: &str) -> Result<Login, PortalError> {
        Err(PortalError::Network("not in this test".into()))
    }
    fn projects(&self, _: &str) -> Result<Vec<ProjectEntry>, PortalError> {
        Ok(vec![ProjectEntry { id: 1, name: "Brand".into(), company_name: "Acme".into(), access: Access::Edit }])
    }
    fn open_project(&self, _: &str, id: i64) -> Result<OpenedProject, PortalError> {
        Ok(OpenedProject { project_id: id, project_name: "Brand".into(), access: Access::Edit, join_url: JOIN.into() })
    }
    fn logout(&self, _: &str, _: &str) -> Result<(), PortalError> {
        Ok(())
    }
}

const JOIN: &str = "figcraft://join?host=127.0.0.1:47900&file=f&token=t";

fn inline(job: Box<dyn FnOnce() + Send + 'static>) {
    job();
}

fn portal() -> Portal {
    Portal::new(BASE, Arc::new(Scripted), Box::new(MemoryStore::default()), Box::new(|| {}), inline, None)
}

/// Remembers the addresses the app was asked to open.
struct Opener(Arc<Mutex<Vec<String>>>);

impl Services for Opener {
    fn open_url(&mut self, url: &str) {
        self.0.lock().unwrap().push(url.to_string());
    }
}

/// The last place `s` is drawn (a popover's text comes after the top bar's).
fn last_center(shapes: &[egui::Shape], s: &str) -> Pos2 {
    let (_, p, size) = texts(shapes).into_iter().rev().find(|(t, _, _)| t == s).unwrap_or_else(|| panic!("no text {s:?}"));
    p + size / 2.0
}

#[test]
fn there_is_no_login_button_without_a_portal() {
    let (mut h, _) = Harness::new();
    h.app.ui.portal.install(Err(ConfigError::NotSet));
    let shapes = h.frames(3);
    assert!(!has_text(&shapes, "Log in"));
}

#[test]
fn the_login_button_opens_a_popover_and_log_in_opens_the_browser() {
    let (mut h, _) = Harness::new();
    let opened = Arc::new(Mutex::new(Vec::new()));
    h.app.services = Box::new(Opener(opened.clone()));
    h.app.ui.portal.install(Ok(portal()));
    let shapes = h.frames(3);
    assert!(has_text(&shapes, "Log in"));
    h.click(text_center(&shapes, "Log in"));
    let shapes = h.frames(3);
    for t in ["Company portal", "Log in to your company portal to open the projects shared with you."] {
        assert!(has_text(&shapes, t), "{t}");
    }
    h.click(last_center(&shapes, "Log in"));
    h.frames(2);
    let opened = opened.lock().unwrap().clone();
    assert_eq!(opened.len(), 1, "{opened:?}");
    let url = opened.first().cloned().unwrap_or_default();
    assert!(url.starts_with(&format!("{BASE}/app-login?challenge=")), "{url}");
    assert_eq!(url.rsplit('=').next().map(str::len), Some(43));
    let shapes = h.frames(2);
    assert!(has_text(&shapes, "Finish logging in in your browser. FigCraft continues here when you are done."));
}

#[test]
fn the_auth_link_goes_to_the_portal_and_other_links_to_the_join_dialog() {
    let (mut h, _) = Harness::new();
    let portal = portal();
    h.app.ui.portal.install(Ok(portal.clone()));
    crate::live::open_invite(&mut h.app, "figcraft://auth?code=abc");
    assert_eq!(h.app.ui.live.join, None, "the auth link isn't a join link");
    assert!(matches!(portal.view().status, Status::Failed(m) if m.contains("No login is waiting")));
    crate::live::open_invite(&mut h.app, "figcraft://join?host=127.0.0.1:47900&file=f&token=t");
    assert_eq!(h.app.ui.live.join.as_deref(), Some("figcraft://join?host=127.0.0.1:47900&file=f&token=t"));
}

#[test]
fn the_name_from_the_login_goes_to_preferences() {
    let (mut h, _) = Harness::new();
    let portal = portal();
    h.app.ui.portal.install(Ok(portal.clone()));
    portal.begin_login().unwrap();
    portal.handle_auth_link("figcraft://auth?code=abc");
    assert_eq!(portal.view().status, Status::SignedIn);
    h.frames(2);
    assert_eq!(h.app.session.prefs.user_name, "Ada Lovelace");
    // The button now carries the name.
    assert!(has_text(&h.frames(1), "Ada Lovelace"));
}

#[test]
fn opening_a_project_joins_its_session() {
    let (mut h, _) = Harness::new();
    let portal = portal();
    h.app.ui.portal.install(Ok(portal.clone()));
    portal.begin_login().unwrap();
    portal.handle_auth_link("figcraft://auth?code=abc");
    h.frames(2);
    assert!(h.app.ui.live.joining.is_none());
    portal.open_project(1);
    h.frames(2);
    assert_eq!(h.app.ui.live.joining.as_ref().map(|(_, link)| link.as_str()), Some(JOIN));
}
