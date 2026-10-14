//! Modal dialogs drawn over everything: provider connection errors, re-auth,
//! plus the generic prompt dialog used by other pages.

use super::menus;
use crate::app::{App, Route};
use crate::theme::{self, ColorExt, Palette};
use crate::widgets::{PillKind, paint_icon, pill_button};
use egui::{
    Align, Color32, Context, CornerRadius, Frame, Id, Key, Layout, Margin, Modal, Sense, Stroke,
    TextEdit, Ui, vec2,
};
use egui_phosphor::regular;

#[derive(Default)]
pub struct State {
    connect: Option<ConnectFailure>,
    auth: Option<AuthDraft>,
    prompt: Option<Prompt>,
}

/// A provider that could not be reached. `mpd` / `subsonic` index the edit
/// state so Retry can test the same server again.
struct ConnectFailure {
    provider: String,
    reason: String,
    attempts: u32,
    mpd: Option<usize>,
    subsonic: Option<usize>,
}

/// A Subsonic server that rejected its credentials.
struct AuthDraft {
    provider: String,
    reason: String,
    index: usize,
    username: String,
    password: String,
}

type SubmitFn = Box<dyn FnOnce(&mut App, String)>;

struct Prompt {
    title: String,
    label: String,
    hint: String,
    submit: String,
    text: String,
    focused: bool,
    on_submit: SubmitFn,
}

/// What a dialog asked for; applied once the dialog stopped borrowing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Act {
    None,
    Dismiss,
    Submit,
    SignIn,
    Retry,
    LocalOnly,
    EditServer,
}

/// A provider connection failed (`ProviderConnectFailed`). Repeated failures
/// of the same provider bump its attempt count; a Subsonic rejection of the
/// credentials opens the re-auth dialog instead.
pub fn on_connect_failed(app: &mut App, provider: &str, reason: &str) {
    let providers = &app.session.providers;
    let id = providers
        .provider_list
        .iter()
        .find(|(id, name)| id == provider || name == provider)
        .map(|(id, _)| id.clone())
        .unwrap_or_else(|| provider.to_owned());
    let mpd = providers.mpd_edit_states.iter().position(|s| s.id == id);
    let subsonic = providers
        .subsonic_edit_states
        .iter()
        .position(|s| s.id == id);
    let state = &mut app.views.overlays;

    if let Some(index) = subsonic.filter(|_| looks_like_auth(reason)) {
        let edit = &app.session.providers.subsonic_edit_states[index];
        state.auth = Some(AuthDraft {
            provider: provider.to_owned(),
            reason: reason.to_owned(),
            index,
            username: edit.username.clone(),
            password: String::new(),
        });
        return;
    }

    match state.connect.as_mut() {
        Some(c) if c.provider == provider => {
            c.attempts += 1;
            c.reason = reason.to_owned();
        }
        _ => {
            state.connect = Some(ConnectFailure {
                provider: provider.to_owned(),
                reason: reason.to_owned(),
                attempts: 1,
                mpd,
                subsonic,
            });
        }
    }
}

/// The OpenSubsonic error text for rejected credentials.
fn looks_like_auth(reason: &str) -> bool {
    let lower = reason.to_lowercase();
    [
        "wrong username",
        "password",
        "unauthorized",
        "401",
        "403",
        "auth",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// A single-line text dialog. `on_submit` receives the trimmed, non-empty text.
pub fn prompt(
    app: &mut App,
    title: &str,
    label: &str,
    hint: &str,
    submit: &str,
    on_submit: impl FnOnce(&mut App, String) + 'static,
) {
    app.views.overlays.prompt = Some(Prompt {
        title: title.to_owned(),
        label: label.to_owned(),
        hint: hint.to_owned(),
        submit: submit.to_owned(),
        text: String::new(),
        focused: false,
        on_submit: Box::new(on_submit),
    });
}

/// Draws the one dialog that is open (prompt, re-auth, then
/// connection error) and carries out its action.
/// Whether a dialog is open. Lets Esc go to the dialog before other sheets.
pub fn active(app: &App) -> bool {
    let state = &app.views.overlays;
    state.connect.is_some() || state.auth.is_some() || state.prompt.is_some()
}

pub fn show(app: &mut App, ctx: &Context) {
    menus::resolve_pending(app);
    let pal = app.pal;
    let has_local = app
        .session
        .providers
        .provider_list
        .iter()
        .any(|(id, _)| id == "local");
    let mut st = std::mem::take(&mut app.views.overlays);
    let escape = ctx.input(|i| i.key_pressed(Key::Escape));

    let act = if st.prompt.is_some() {
        prompt_dialog(ctx, pal, &mut st)
    } else if st.auth.is_some() {
        auth_dialog(ctx, pal, &mut st)
    } else if st.connect.is_some() {
        connect_dialog(ctx, pal, &st, has_local)
    } else {
        Act::None
    };
    let act = if escape && act == Act::None && st.any() {
        Act::Dismiss
    } else {
        act
    };

    app.views.overlays = st;
    apply(app, act);
}

impl State {
    fn any(&self) -> bool {
        self.prompt.is_some() || self.auth.is_some() || self.connect.is_some()
    }
}

fn apply(app: &mut App, act: Act) {
    match act {
        Act::None => {}
        Act::Dismiss => {
            let st = &mut app.views.overlays;
            if st.prompt.take().is_none() && st.auth.take().is_none() {
                st.connect = None;
            }
        }
        Act::Submit => {
            if let Some(p) = app.views.overlays.prompt.take() {
                let text = p.text.trim().to_owned();
                if !text.is_empty() {
                    (p.on_submit)(app, text);
                }
            }
        }
        Act::SignIn => {
            if let Some(d) = app.views.overlays.auth.take() {
                if let Some(edit) = app.session.providers.subsonic_edit_states.get_mut(d.index) {
                    edit.username = d.username;
                    edit.password = d.password;
                }
                let reinit = app.session.save_subsonic_server(d.index);
                app.run(reinit);
            }
        }
        Act::Retry => {
            if let Some(c) = app.views.overlays.connect.take() {
                let effect = match (c.mpd, c.subsonic) {
                    (Some(i), _) => app.session.providers.test_mpd_connection(i),
                    (None, Some(i)) => app.session.providers.test_subsonic_connection(i),
                    (None, None) => return,
                };
                app.run(effect);
            }
        }
        Act::LocalOnly => {
            app.views.overlays.connect = None;
            let local = app
                .session
                .providers
                .provider_list
                .iter()
                .position(|(id, _)| id == "local");
            if let Some(i) = local {
                let update = app.session.switch_provider(i);
                app.run(update);
            }
        }
        Act::EditServer => {
            app.views.overlays.connect = None;
            app.navigate(Route::Settings);
        }
    }
}

const SCRIM: Color32 = Color32::from_rgba_premultiplied(16, 15, 14, 209);

/// Draws a centred card over a scrim. Returns the action the body picked, or
/// `Dismiss` when the scrim is clicked.
fn dialog(
    ctx: &Context,
    id: &str,
    pal: Palette,
    width: f32,
    body: impl FnOnce(&mut Ui) -> Act,
) -> Act {
    let mut act = Act::None;
    let response = Modal::new(Id::new(id))
        .backdrop_color(SCRIM)
        .frame(Frame::NONE)
        .show(ctx, |ui| {
            Frame::new()
                .fill(pal.surface_1)
                .stroke(Stroke::new(1.0, pal.stroke_strong))
                .corner_radius(CornerRadius::same(12))
                .inner_margin(Margin::same(20))
                .shadow(theme::shadow([0, 24], 60, 140))
                .show(ui, |ui| {
                    ui.set_width(width - 40.0);
                    act = body(ui);
                });
        });
    if act == Act::None && response.backdrop_response.clicked() {
        return Act::Dismiss;
    }
    act
}

/// A 40px tinted disc with a centred glyph, used as the dialog's header mark.
fn badge(ui: &mut Ui, glyph: &str, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(40.0, 40.0), Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), 20.0, color.alpha(0.14));
    paint_icon(ui.painter(), rect.center(), glyph, 20.0, color, false);
}

fn title(ui: &mut Ui, pal: Palette, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .font(theme::semibold(16.0))
            .color(pal.text),
    );
}

fn body_text(ui: &mut Ui, pal: Palette, text: &str) {
    ui.add(
        egui::Label::new(
            egui::RichText::new(text)
                .font(theme::regular(12.5))
                .color(pal.text_2),
        )
        .wrap(),
    );
}

/// Right-aligned footer: the buttons in `(label, icon, kind, action)` order,
/// first entry rightmost.
fn footer(ui: &mut Ui, buttons: &[(&str, Option<&str>, PillKind, Act)]) -> Act {
    let mut act = Act::None;
    ui.add_space(16.0);
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        for (label, icon, kind, pick) in buttons {
            if pill_button(ui, label, *icon, *kind, 30.0, 0.0).clicked() {
                act = *pick;
            }
        }
    });
    act
}

fn connect_dialog(ctx: &Context, pal: Palette, st: &State, has_local: bool) -> Act {
    let Some(c) = st.connect.as_ref() else {
        return Act::None;
    };
    dialog(ctx, "overlay_connect", pal, 460.0, |ui| {
        ui.horizontal_top(|ui| {
            badge(ui, regular::WIFI_SLASH, pal.danger);
            ui.vertical(|ui| {
                ui.add_space(2.0);
                title(ui, pal, &format!("Can't reach {}", c.provider));
                ui.add_space(4.0);
                body_text(ui, pal, &c.reason);
            });
        });
        if c.attempts > 1 {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(format!("{} attempts failed", c.attempts))
                    .font(theme::mono(11.0))
                    .color(pal.text_3),
            );
        }
        let mut buttons = vec![
            (
                "Retry",
                Some(regular::ARROW_CLOCKWISE),
                PillKind::Primary,
                Act::Retry,
            ),
            ("Edit server", None, PillKind::Ghost, Act::EditServer),
        ];
        if has_local {
            buttons.push(("Show local only", None, PillKind::Ghost, Act::LocalOnly));
        }
        footer(ui, &buttons)
    })
}

fn auth_dialog(ctx: &Context, pal: Palette, st: &mut State) -> Act {
    let Some(d) = st.auth.as_mut() else {
        return Act::None;
    };
    let name = d.provider.clone();
    let reason = d.reason.clone();
    dialog(ctx, "overlay_auth", pal, 400.0, |ui| {
        ui.horizontal_top(|ui| {
            badge(ui, regular::LOCK_KEY, pal.warn);
            ui.vertical(|ui| {
                ui.add_space(2.0);
                title(ui, pal, &format!("Sign in to {name}"));
                ui.add_space(4.0);
                body_text(ui, pal, "The server rejected these credentials.");
            });
        });
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(reason)
                .font(theme::mono(11.0))
                .color(pal.text_3),
        );
        ui.add_space(12.0);
        field_label(ui, pal, "Username");
        crate::widgets::text_field(ui, &mut d.username, "Username", 360.0);
        ui.add_space(8.0);
        field_label(ui, pal, "Password");
        ui.add(
            TextEdit::singleline(&mut d.password)
                .password(true)
                .hint_text(egui::RichText::new("Password").color(pal.text_3))
                .desired_width(360.0)
                .margin(vec2(10.0, 6.0))
                .font(theme::regular(13.0))
                .background_color(pal.surface_2),
        );
        let can_sign_in = !d.username.trim().is_empty();
        let mut act = footer(
            ui,
            &[
                ("Sign in", None, PillKind::Primary, Act::SignIn),
                ("Cancel", None, PillKind::Ghost, Act::Dismiss),
            ],
        );
        if act == Act::SignIn && !can_sign_in {
            act = Act::None;
        }
        act
    })
}

fn field_label(ui: &mut Ui, pal: Palette, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .font(theme::medium(11.5))
            .color(pal.text_3),
    );
}

fn prompt_dialog(ctx: &Context, pal: Palette, st: &mut State) -> Act {
    let Some(p) = st.prompt.as_mut() else {
        return Act::None;
    };
    dialog(ctx, "overlay_prompt", pal, 400.0, |ui| {
        title(ui, pal, &p.title);
        ui.add_space(12.0);
        field_label(ui, pal, &p.label);
        let edit = TextEdit::singleline(&mut p.text)
            .hint_text(egui::RichText::new(&p.hint).color(pal.text_3))
            .desired_width(360.0)
            .margin(vec2(10.0, 6.0))
            .font(theme::regular(13.0))
            .background_color(pal.surface_2);
        let response = ui.add(edit);
        if !p.focused {
            response.request_focus();
            p.focused = true;
        }
        let enter = response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        let can_submit = !p.text.trim().is_empty();
        let mut act = footer(
            ui,
            &[
                (p.submit.as_str(), None, PillKind::Primary, Act::Submit),
                ("Cancel", None, PillKind::Ghost, Act::Dismiss),
            ],
        );
        if (enter && can_submit) || (act == Act::Submit && !can_submit) {
            act = if can_submit { Act::Submit } else { Act::None };
        }
        act
    })
}
