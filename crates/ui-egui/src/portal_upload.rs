//! «Upload current file to portal…»: an Admin or CompanyAdmin sends the open document to the
//! company portal as a new project. The profile menu (`portal::profile_top`) offers the entry; the
//! dialog asks for the project name (and, for an Admin, the company), then the document is taken
//! exactly as Save as would write it (`file.saveAs` with `data`, which changes nothing in the
//! document) and `figiran_portal::Portal` uploads it on a background thread. This file only draws
//! the dialog and words the outcome; the portal's server key never comes near the app.

use std::time::Duration;

use egui::{Context, Id, Ui};
use figiran_portal::session::Account;
use figiran_portal::upload::Companies;
use figiran_portal::{Portal, Status, UploadError, UploadJob, UploadPhase, UploadRole, UploadView, View};
use serde_json::json;

use crate::FigApp;
use crate::dialogs;
use crate::i18n::{tr, trf};
use crate::theme::{Tokens, semibold};
use crate::widgets;

/// The dialog's width.
const WIDTH: f32 = 340.0;

/// The upload dialog's state.
#[derive(Default)]
pub struct UploadDialog {
    open: bool,
    name: String,
    company: Option<i64>,
    /// Why Upload did nothing (no name, no company, the document couldn't be taken).
    problem: Option<String>,
}

impl UploadDialog {
    pub fn is_open(&self) -> bool {
        self.open
    }
}

/// What the signed-in person may do: Admin and CompanyAdmin upload, everyone else doesn't.
pub fn role_of(view: &View) -> Option<UploadRole> {
    if view.status != Status::SignedIn {
        return None;
    }
    view.account.as_ref().and_then(|a| UploadRole::from_name(&a.role))
}

/// The profile menu offers the entry: an uploader role and a document to send.
pub fn offered(view: &View, has_document: bool) -> bool {
    has_document && role_of(view).is_some()
}

/// What stops Upload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Problem {
    NoName,
    NoCompany,
}

impl Problem {
    fn text(self) -> &'static str {
        match self {
            Problem::NoName => tr("Enter a project name."),
            Problem::NoCompany => tr("Choose a company."),
        }
    }
}

/// What stops Upload (`None`: ready).
pub fn problem_with(role: UploadRole, name: &str, company: Option<i64>) -> Option<Problem> {
    if figiran_portal::upload::clean_name(name).is_empty() {
        return Some(Problem::NoName);
    }
    if role == UploadRole::Admin && company.is_none() {
        return Some(Problem::NoCompany);
    }
    None
}

/// The person's words for why an upload failed.
pub fn error_text(error: &UploadError) -> String {
    match error {
        UploadError::Unauthorized => tr("Your session ended. Log in again.").to_string(),
        UploadError::Forbidden => tr("Your account isn't allowed to upload projects to this company.").to_string(),
        UploadError::Invalid(reason) => trf("The portal refused the file: {reason}", &[("reason", reason)]),
        UploadError::Conflict(reason) => trf("The portal reports a conflict: {reason}", &[("reason", reason)]),
        UploadError::TooLarge | UploadError::DocumentTooLarge => tr("The file is larger than the portal accepts (200 MB).").to_string(),
        UploadError::Server { status, message } => {
            trf("The portal had a problem (status {status}): {reason}", &[("status", status), ("reason", message)])
        }
        UploadError::Network(reason) => trf("The portal can't be reached: {reason}", &[("reason", reason)]),
        UploadError::BadResponse(_) => tr("The portal's answer couldn't be understood.").to_string(),
        UploadError::Cancelled => tr("Upload cancelled.").to_string(),
        UploadError::Local(reason) => trf("The file couldn't be prepared: {reason}", &[("reason", reason)]),
    }
}

/// The profile menu's row. Returns true when it was pressed (the dialog is then open).
pub fn menu_entry(app: &mut FigApp, ui: &mut Ui, portal: &Portal, view: &View) -> bool {
    let has_document = app.session.doc().is_ok();
    if !offered(view, has_document) {
        return false;
    }
    if !crate::right::menu_item(ui, tr("Upload current file to portal…"), "") {
        return false;
    }
    open(app, portal, view);
    ui.close();
    true
}

/// Open the dialog for the open document (named after it), and load the companies an Admin picks from.
fn open(app: &mut FigApp, portal: &Portal, view: &View) {
    let name = app.session.doc().map(|d| d.name.clone()).unwrap_or_default();
    let dialog = &mut app.ui.portal.upload;
    dialog.open = true;
    dialog.name = name;
    dialog.company = None;
    dialog.problem = None;
    portal.reset_upload();
    if role_of(view) == Some(UploadRole::Admin) && matches!(portal.upload_view().companies, Companies::NotLoaded | Companies::Failed(_)) {
        portal.refresh_companies();
    }
}

/// Draw the dialog while it is open (once per frame).
pub fn show(app: &mut FigApp, ctx: &Context, portal: &Portal, view: &View) {
    if !app.ui.portal.upload.open {
        return;
    }
    let Some(role) = role_of(view) else {
        // Logged out (or not an uploader any more): nothing to send with.
        portal.cancel_upload();
        app.ui.portal.upload.open = false;
        return;
    };
    let up = portal.upload_view();
    if up.phase.is_active() {
        ctx.request_repaint_after(Duration::from_millis(100));
    }
    let title = tr("Upload to portal");
    dialogs::window(title, ctx).show(ctx, |ui| {
        ui.set_width(WIDTH);
        ui.spacing_mut().item_spacing.y = 8.0;
        body(app, ui, portal, role, view.account.as_ref(), &up);
    });
}

fn body(app: &mut FigApp, ui: &mut Ui, portal: &Portal, role: UploadRole, account: Option<&Account>, up: &UploadView) {
    let t = Tokens::get(ui.ctx());
    let idle = !up.phase.is_active() && !matches!(up.phase, UploadPhase::Accepted(_));
    if idle {
        form(app, ui, role, account, up);
    }
    match &up.phase {
        UploadPhase::Preparing => widgets::label(ui, tr("Preparing the file…"), t.text_secondary),
        UploadPhase::Uploading => {
            widgets::label(ui, tr("Uploading to the portal…"), t.text_secondary);
            ui.add(egui::ProgressBar::new(up.fraction()).desired_width(WIDTH).show_percentage());
        }
        UploadPhase::Accepted(project) => {
            ui.label(egui::RichText::new(&project.name).font(semibold(12.0)));
            widgets::label(
                ui,
                tr("The project was uploaded and is being imported. It appears in your project list when it is ready."),
                t.text_secondary,
            );
        }
        UploadPhase::Failed(error) => widgets::label(ui, &error_text(error), t.danger),
        UploadPhase::Cancelled => widgets::label(ui, tr("Upload cancelled."), t.text_secondary),
        UploadPhase::Idle => {}
    }
    if let Some(problem) = app.ui.portal.upload.problem.as_deref().filter(|_| idle) {
        widgets::label(ui, problem, t.danger);
    }
    ui.horizontal(|ui| {
        if up.phase.is_active() {
            if widgets::button(ui, tr("Cancel"), false, true).clicked() {
                portal.cancel_upload();
            }
            return;
        }
        if idle && widgets::button(ui, tr("Upload"), true, true).clicked() {
            begin(app, portal, role);
        }
        if widgets::button(ui, tr("Close"), false, true).clicked() {
            portal.reset_upload();
            app.ui.portal.upload.open = false;
        }
    });
}

/// The project name, and the company: an Admin chooses, a CompanyAdmin's is their own.
fn form(app: &mut FigApp, ui: &mut Ui, role: UploadRole, account: Option<&Account>, up: &UploadView) {
    let t = Tokens::get(ui.ctx());
    let dialog = &mut app.ui.portal.upload;
    widgets::label(ui, tr("Project name"), t.text_secondary);
    widgets::text_input(ui, Id::unique("portal_upload_name"), &mut dialog.name, WIDTH, tr("Project name"));
    match role {
        UploadRole::CompanyAdmin => {
            let company = account.map(|a| a.company.as_str()).unwrap_or_default();
            widgets::label(ui, &trf("Company: {company}", &[("company", &company)]), t.text_secondary);
        }
        UploadRole::Admin => {
            widgets::label(ui, tr("Company"), t.text_secondary);
            match &up.companies {
                Companies::NotLoaded | Companies::Loading => widgets::label(ui, tr("Loading companies…"), t.text_secondary),
                Companies::Failed(message) => widgets::label(ui, message, t.danger),
                Companies::Loaded(list) => {
                    egui::ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
                        for c in list {
                            if ui.selectable_label(dialog.company == Some(c.id), &c.name).clicked() {
                                dialog.company = Some(c.id);
                                dialog.problem = None;
                            }
                        }
                    });
                }
            }
        }
    }
}

/// Take the document the way Save as writes it, and hand it to the portal.
fn begin(app: &mut FigApp, portal: &Portal, role: UploadRole) {
    let (name, company) = (app.ui.portal.upload.name.clone(), app.ui.portal.upload.company);
    if let Some(problem) = problem_with(role, &name, company) {
        app.ui.portal.upload.problem = Some(problem.text().to_string());
        return;
    }
    app.ui.portal.upload.problem = None;
    // `file.saveAs` with `data` encodes the document and writes nothing: the file keeps its saved state.
    let Some(saved) = app.run("file.saveAs", json!({"data": true})) else { return };
    let Some(encoded) = saved["data"].as_str() else {
        app.ui.portal.upload.problem = Some(tr("The file couldn't be prepared: the document couldn't be saved.").to_string());
        return;
    };
    let company_id = (role == UploadRole::Admin).then_some(company).flatten();
    portal.start_upload(UploadJob { encoded: encoded.to_string(), name, company_id, temp_dir: std::env::temp_dir() });
}
