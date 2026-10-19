//! grammalocal-popup: a small GTK4 window that checks the current selection with grammalocald.
//! Enter or Apply copies the corrected text to the clipboard (or, in replace mode, pastes it)
//! and hides the window.

mod api;
mod portal;
mod setup;
mod text;

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gtk4::prelude::*;
use gtk4::{gdk, gio, glib, pango};
use gtk4::{
    Application, ApplicationWindow, Box as GtkBox, Button, DropDown, EventControllerKey,
    GestureClick, Label, Orientation, PolicyType, Popover, ScrolledWindow, TextBuffer, TextTag,
    TextView, TextWindowType, WrapMode,
};

const APP_ID: &str = "dev.grammalocal.popup";
const DEBOUNCE: Duration = Duration::from_millis(500);
/// After copying, the process stays alive so it keeps owning the clipboard.
const LINGER: Duration = Duration::from_secs(600);
/// Replace mode waits this long after hiding the window, so the previous app regains focus.
const PASTE_DELAY: Duration = Duration::from_millis(250);
const MAX_SUGGESTIONS: usize = 5;

/// One finding, with its span in characters of the buffer text.
struct Found {
    spelling: bool,
    start: usize,
    len: usize,
    rule_id: String,
    matched: String,
    message: String,
    suggestions: Vec<String>,
}

struct State {
    app: Application,
    window: ApplicationWindow,
    view: TextView,
    buffer: TextBuffer,
    /// Red underline for spelling errors.
    tag: TextTag,
    /// Blue underline for grammar errors.
    grammar_tag: TextTag,
    status: Label,
    lang: DropDown,
    popover: Popover,
    client: Arc<api::Client>,
    apply_replace: bool,
    matches: RefCell<Vec<Found>>,
    ignored: RefCell<HashSet<(String, String)>>,
    /// Bumped on every edit or language change. Stale results are dropped.
    generation: Cell<u64>,
    /// Set while the window loads text itself, so the change handler does not schedule a check.
    suppress: Cell<bool>,
    finished: Cell<bool>,
}

fn main() -> glib::ExitCode {
    if let Some(ok) = setup::run_from_args() {
        return if ok {
            glib::ExitCode::SUCCESS
        } else {
            glib::ExitCode::FAILURE
        };
    }
    let t0 = Instant::now();
    let cfg = grammalocald::config::config_dir()
        .and_then(|dir| grammalocald::config::Config::load_or_create(&dir))
        .unwrap_or_default();

    let client = Arc::new(api::Client::new(cfg.port, cfg.token.clone()));
    let lang_index = match cfg.default_language.as_str() {
        "fr" => 1,
        "en" => 2,
        _ => 0,
    };
    let apply_replace = cfg.apply_mode == "replace";

    let app = Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.connect_activate(move |app| {
        build(app, t0, client.clone(), lang_index, apply_replace);
    });
    app.run()
}

fn build(
    app: &Application,
    t0: Instant,
    client: Arc<api::Client>,
    lang_index: u32,
    apply_replace: bool,
) {
    let window = ApplicationWindow::builder()
        .application(app)
        .title("Grammalocal")
        .default_width(680)
        .default_height(440)
        .build();

    let lang = DropDown::from_strings(&["Auto", "Français", "English"]);
    lang.set_selected(lang_index);
    let status = Label::new(None);
    status.set_hexpand(true);
    status.set_xalign(0.0);
    let accept_all = Button::with_label("Accept all");
    let top = GtkBox::new(Orientation::Horizontal, 8);
    top.append(&lang);
    top.append(&status);
    top.append(&accept_all);

    let buffer = TextBuffer::new(None);
    let tag = TextTag::builder()
        .name("error")
        .underline(pango::Underline::Error)
        .underline_rgba(&gdk::RGBA::new(0.85, 0.1, 0.1, 1.0))
        .build();
    buffer.tag_table().add(&tag);
    let grammar_tag = TextTag::builder()
        .name("grammar")
        .underline(pango::Underline::Error)
        .underline_rgba(&gdk::RGBA::new(0.15, 0.39, 0.92, 1.0))
        .build();
    buffer.tag_table().add(&grammar_tag);

    let view = TextView::with_buffer(&buffer);
    view.set_wrap_mode(WrapMode::WordChar);
    view.set_accepts_tab(false);
    view.set_left_margin(8);
    view.set_right_margin(8);
    view.set_top_margin(8);
    view.set_bottom_margin(8);
    let scroller = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Never)
        .vexpand(true)
        .child(&view)
        .build();

    let popover = Popover::new();
    popover.set_parent(&view);
    // Not autohide: an autohide popover is modal and takes the keyboard, so typing would
    // need a click to dismiss it first. It closes on edits, clicks and Escape instead.
    popover.set_autohide(false);
    popover.set_can_focus(false);

    let cancel = Button::with_label("Cancel");
    let apply = Button::with_label("Apply");
    apply.add_css_class("suggested-action");
    apply.set_tooltip_text(Some(if apply_replace {
        "Replace the selection"
    } else {
        "Copy corrected text to clipboard"
    }));
    let bottom = GtkBox::new(Orientation::Horizontal, 8);
    bottom.set_halign(gtk4::Align::End);
    bottom.append(&cancel);
    bottom.append(&apply);

    let root = GtkBox::new(Orientation::Vertical, 8);
    pad(&root, 12);
    root.append(&top);
    root.append(&scroller);
    root.append(&bottom);
    window.set_child(Some(&root));

    let state = Rc::new(State {
        app: app.clone(),
        window: window.clone(),
        view: view.clone(),
        buffer: buffer.clone(),
        tag,
        grammar_tag,
        status: status.clone(),
        lang: lang.clone(),
        popover,
        client,
        apply_replace,
        matches: RefCell::new(Vec::new()),
        ignored: RefCell::new(HashSet::new()),
        generation: Cell::new(0),
        suppress: Cell::new(false),
        finished: Cell::new(false),
    });

    // Edits keep the previous findings, moved along with the text, until the next check
    // replaces them. Only findings the edit touches are dropped.
    {
        let s = state.clone();
        buffer.connect_insert_text(move |_, at, text| {
            shift_matches(&s, at.offset() as usize, 0, text.chars().count());
        });
    }
    {
        let s = state.clone();
        buffer.connect_delete_range(move |_, a, b| {
            let (a, b) = (a.offset() as usize, b.offset() as usize);
            shift_matches(&s, a.min(b), a.abs_diff(b), 0);
        });
    }
    {
        let s = state.clone();
        buffer.connect_changed(move |_| {
            if !s.suppress.get() {
                s.popover.popdown();
                retag(&s);
                schedule(&s);
            }
        });
    }
    {
        let s = state.clone();
        lang.connect_selected_notify(move |_| schedule(&s));
    }
    {
        let s = state.clone();
        window.connect_is_active_notify(move |w| {
            if !w.is_active() {
                s.popover.popdown();
            }
        });
    }
    {
        let s = state.clone();
        accept_all.connect_clicked(move |_| accept_all_fixes(&s));
    }
    {
        let s = state.clone();
        apply.connect_clicked(move |_| finish(&s));
    }
    {
        let s = state.clone();
        cancel.connect_clicked(move |_| s.app.quit());
    }
    {
        let click = GestureClick::new();
        let s = state.clone();
        click.connect_pressed(move |g, _, x, y| {
            // The popover is a child of the view, so presses on its buttons bubble up here.
            // Handling them would pop it down before the button's `clicked` fires.
            let on_popover = g
                .current_event()
                .and_then(|e| e.surface())
                .zip(s.popover.surface())
                .is_some_and(|(a, b)| a == b);
            if !on_popover {
                on_click(&s, x, y);
            }
        });
        view.add_controller(click);
    }
    {
        let keys = EventControllerKey::new();
        keys.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let s = state.clone();
        keys.connect_key_pressed(move |_, key, _, _| match key {
            gdk::Key::Escape if s.popover.is_visible() => {
                s.popover.popdown();
                glib::Propagation::Stop
            }
            gdk::Key::Escape => {
                s.app.quit();
                glib::Propagation::Stop
            }
            gdk::Key::Return | gdk::Key::KP_Enter => {
                finish(&s);
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        });
        window.add_controller(keys);
    }

    window.present();
    if std::env::var("GRAMMALOCAL_DEBUG").as_deref() == Ok("1") {
        eprintln!(
            "grammalocal-popup: window presented after {:?}",
            t0.elapsed()
        );
    }

    let s = state.clone();
    glib::MainContext::default().spawn_local(async move {
        load_selection(&s).await;
    });
}

/// Resolves once `ready(obj)` holds, re-testing whenever `connect` fires, or after
/// 500 ms so the popup never hangs when the condition can't become true.
async fn wait_until<O: IsA<glib::Object>>(
    obj: &O,
    ready: impl Fn(&O) -> bool + 'static,
    connect: impl FnOnce(&O, Box<dyn Fn(&O) + 'static>) -> glib::SignalHandlerId,
) {
    if ready(obj) {
        return;
    }
    let (tx, rx) = futures_channel::oneshot::channel::<()>();
    let tx = RefCell::new(Some(tx));
    let id = connect(
        obj,
        Box::new(move |o| {
            if ready(o) {
                if let Some(tx) = tx.borrow_mut().take() {
                    let _ = tx.send(());
                }
            }
        }),
    );
    let timeout = glib::timeout_future(Duration::from_millis(500));
    futures_util::future::select(rx, timeout).await;
    obj.disconnect(id);
}

/// Reads PRIMARY, falling back to CLIPBOARD, and starts the first check.
async fn load_selection(s: &Rc<State>) {
    let Some(display) = gdk::Display::default() else {
        s.status.set_text("No display");
        return;
    };
    // Wayland only announces selections to the client holding keyboard focus,
    // so wait for the window to become active, then for the selection offer.
    wait_until(
        &s.window,
        |w| w.is_active(),
        |w, f| w.connect_is_active_notify(f),
    )
    .await;
    let has_text = |c: &gdk::Clipboard| {
        let f = c.formats();
        f.contains_type(glib::Type::STRING)
            || f.contain_mime_type("text/plain;charset=utf-8")
            || f.contain_mime_type("text/plain")
    };
    let primary_cb = display.primary_clipboard();
    wait_until(&primary_cb, has_text, |c, f| c.connect_changed(f)).await;
    let primary = primary_cb.read_text_future().await.ok().flatten();
    let text = match primary.filter(|t| !t.trim().is_empty()) {
        Some(t) => t.to_string(),
        None => display
            .clipboard()
            .read_text_future()
            .await
            .ok()
            .flatten()
            .map(|t| t.to_string())
            .unwrap_or_default(),
    };

    if std::env::var("GRAMMALOCAL_DEBUG").as_deref() == Ok("1") {
        eprintln!("grammalocal-popup: loaded {} chars", text.chars().count());
    }
    s.suppress.set(true);
    s.buffer.set_text(&text);
    s.suppress.set(false);
    if text.trim().is_empty() {
        s.status.set_text("Nothing selected");
    } else {
        run_check(s);
    }
}

/// Restarts the debounce timer. The previous marks stay visible until the check returns.
fn schedule(s: &Rc<State>) {
    let gen = s.generation.get() + 1;
    s.generation.set(gen);
    update_status(s);
    let s2 = s.clone();
    glib::timeout_add_local_once(DEBOUNCE, move || {
        if s2.generation.get() == gen {
            run_check(&s2);
        }
    });
}

fn run_check(s: &Rc<State>) {
    let text = buffer_text(&s.buffer);
    if text.trim().is_empty() {
        s.status.set_text("Nothing to check");
        return;
    }
    let lang = lang_code(s.lang.selected());
    let gen = s.generation.get();
    s.status.set_text("Checking…");

    let client = s.client.clone();
    let s2 = s.clone();
    glib::MainContext::default().spawn_local(async move {
        let t = text.clone();
        let res = gio::spawn_blocking(move || client.check(&t, lang)).await;
        if s2.generation.get() != gen {
            return;
        }
        match res {
            Ok(Ok(matches)) => apply_results(&s2, &text, matches),
            Ok(Err(e)) => s2.status.set_text(&e),
            Err(_) => s2.status.set_text("internal error"),
        }
    });
}

fn apply_results(s: &Rc<State>, text: &str, matches: Vec<api::Match>) {
    let mut found = Vec::new();
    {
        let ignored = s.ignored.borrow();
        for m in matches {
            let Some((start, len)) = text::utf16_span_to_chars(text, m.offset, m.length) else {
                continue;
            };
            let matched: String = text.chars().skip(start).take(len).collect();
            if ignored.contains(&(m.rule.id.clone(), matched.clone())) {
                continue;
            }
            found.push(Found {
                spelling: m.rule.issue_type == "misspelling",
                start,
                len,
                rule_id: m.rule.id,
                matched,
                message: m.message,
                suggestions: m
                    .replacements
                    .into_iter()
                    .take(MAX_SUGGESTIONS)
                    .map(|r| r.value)
                    .collect(),
            });
        }
    }
    *s.matches.borrow_mut() = found;
    retag(s);
    update_status(s);
}

fn update_status(s: &Rc<State>) {
    let n = s.matches.borrow().len();
    match n {
        0 => s.status.set_text("No issues"),
        1 => s.status.set_text("1 issue"),
        n => s.status.set_text(&format!("{n} issues")),
    }
}

/// Keeps findings in step with an edit that replaces `removed` characters at `at` by
/// `inserted` characters. Runs before the buffer changes, so it must not touch the buffer.
fn shift_matches(s: &Rc<State>, at: usize, removed: usize, inserted: usize) {
    if s.suppress.get() {
        s.matches.borrow_mut().clear();
        return;
    }
    s.matches.borrow_mut().retain_mut(|f| {
        match text::shift_span(f.start, f.len, at, removed, inserted) {
            Some(start) => {
                f.start = start;
                true
            }
            None => false,
        }
    });
}

/// Removes the error tag everywhere, then tags every current finding.
fn retag(s: &Rc<State>) {
    let (a, b) = (s.buffer.start_iter(), s.buffer.end_iter());
    s.buffer.remove_tag(&s.tag, &a, &b);
    s.buffer.remove_tag(&s.grammar_tag, &a, &b);
    for f in s.matches.borrow().iter() {
        let st = s.buffer.iter_at_offset(f.start as i32);
        let en = s.buffer.iter_at_offset((f.start + f.len) as i32);
        let tag = if f.spelling { &s.tag } else { &s.grammar_tag };
        s.buffer.apply_tag(tag, &st, &en);
    }
}

fn on_click(s: &Rc<State>, x: f64, y: f64) {
    let (bx, by) = s
        .view
        .window_to_buffer_coords(TextWindowType::Widget, x as i32, y as i32);
    let Some(iter) = s.view.iter_at_location(bx, by) else {
        s.popover.popdown();
        return;
    };
    let off = iter.offset() as usize;
    let idx = s
        .matches
        .borrow()
        .iter()
        .position(|f| off >= f.start && off < f.start + f.len);
    let Some(idx) = idx else {
        s.popover.popdown();
        return;
    };
    let rect = s.view.iter_location(&iter);
    let (wx, wy) = s
        .view
        .buffer_to_window_coords(TextWindowType::Widget, rect.x(), rect.y());
    let anchor = gdk::Rectangle::new(wx, wy, rect.width(), rect.height());
    show_popover(s, idx, &anchor);
}

fn show_popover(s: &Rc<State>, idx: usize, anchor: &gdk::Rectangle) {
    // Findings can move or be replaced while the popover is open, so its buttons refer to
    // the finding by its span rather than by index.
    let (message, suggestions, span) = {
        let m = s.matches.borrow();
        let f = &m[idx];
        (f.message.clone(), f.suggestions.clone(), (f.start, f.len))
    };

    let content = GtkBox::new(Orientation::Vertical, 4);
    pad(&content, 8);
    let msg = Label::new(Some(&message));
    msg.set_wrap(true);
    msg.set_max_width_chars(40);
    msg.set_xalign(0.0);
    content.append(&msg);

    if suggestions.is_empty() {
        content.append(&Label::new(Some("No suggestion")));
    }
    for sug in suggestions {
        let btn = Button::with_label(&sug);
        let s2 = s.clone();
        btn.connect_clicked(move |_| {
            s2.popover.popdown();
            apply_suggestion(&s2, span, &sug);
        });
        content.append(&btn);
    }

    let ignore_btn = Button::with_label("Ignore");
    {
        let s2 = s.clone();
        ignore_btn.connect_clicked(move |_| {
            s2.popover.popdown();
            ignore(&s2, span);
        });
    }
    content.append(&ignore_btn);

    s.popover.set_child(Some(&content));
    s.popover.set_pointing_to(Some(anchor));
    s.popover.popup();
}

/// Index of the current finding covering exactly `span`, if it still exists.
fn find_span(s: &Rc<State>, span: (usize, usize)) -> Option<usize> {
    s.matches
        .borrow()
        .iter()
        .position(|f| (f.start, f.len) == span)
}

fn apply_suggestion(s: &Rc<State>, span: (usize, usize), repl: &str) {
    if find_span(s, span).is_none() {
        return;
    }
    let (start, len) = span;
    let mut a = s.buffer.iter_at_offset(start as i32);
    let mut b = s.buffer.iter_at_offset((start + len) as i32);
    s.buffer.delete(&mut a, &mut b);
    let mut at = s.buffer.iter_at_offset(start as i32);
    s.buffer.insert(&mut at, repl);
}

fn ignore(s: &Rc<State>, span: (usize, usize)) {
    let removed = find_span(s, span).map(|idx| s.matches.borrow_mut().remove(idx));
    if let Some(f) = removed {
        s.ignored.borrow_mut().insert((f.rule_id, f.matched));
        retag(s);
        update_status(s);
    }
}

fn accept_all_fixes(s: &Rc<State>) {
    let edits: Vec<text::Edit> = s
        .matches
        .borrow()
        .iter()
        .filter_map(|f| {
            f.suggestions.first().map(|r| text::Edit {
                start: f.start,
                len: f.len,
                replacement: r.clone(),
            })
        })
        .collect();
    if edits.is_empty() {
        return;
    }
    // Edit in place rather than resetting the text, so findings without a suggestion stay marked.
    let len = s.buffer.char_count() as usize;
    s.buffer.begin_user_action();
    for edit in text::applicable_edits(&edits, len) {
        let mut a = s.buffer.iter_at_offset(edit.start as i32);
        let mut b = s.buffer.iter_at_offset((edit.start + edit.len) as i32);
        s.buffer.delete(&mut a, &mut b);
        let mut at = s.buffer.iter_at_offset(edit.start as i32);
        s.buffer.insert(&mut at, &edit.replacement);
    }
    s.buffer.end_user_action();
}

/// Copies the buffer to the clipboard, hides the window, and keeps the process alive
/// until another application takes clipboard ownership or the linger timeout passes.
fn finish(s: &Rc<State>) {
    if s.finished.replace(true) {
        return;
    }
    let text = buffer_text(&s.buffer);
    let clipboard = s.view.clipboard();
    clipboard.set_text(&text);

    s.popover.popdown();
    s.window.set_visible(false);

    let app = s.app.clone();
    clipboard.connect_changed(move |cb| {
        if !cb.is_local() {
            app.quit();
        }
    });
    let app = s.app.clone();
    glib::timeout_add_local_once(LINGER, move || app.quit());
    if s.apply_replace {
        let s2 = s.clone();
        glib::MainContext::default().spawn_local(async move { paste_replacement(&s2).await });
    }
}

/// Replace mode: pastes into the previously focused app. Falls back to ydotool, then to
/// a notification asking for a manual Ctrl+V. The clipboard stays owned while lingering.
async fn paste_replacement(s: &Rc<State>) {
    glib::timeout_future(PASTE_DELAY).await;
    let err = match gio::spawn_blocking(portal::paste_ctrl_v).await {
        Ok(Ok(())) => return,
        Ok(Err(e)) => e,
        Err(_) => "internal error".to_string(),
    };
    eprintln!("grammalocal-popup: portal paste failed: {err}");
    if gio::spawn_blocking(portal::ydotool_paste)
        .await
        .unwrap_or(false)
    {
        return;
    }
    let note = gio::Notification::new("Grammalocal");
    note.set_body(Some("Corrected text copied \u{2014} press Ctrl+V"));
    s.app.send_notification(Some("corrected-text"), &note);
}

fn lang_code(index: u32) -> &'static str {
    match index {
        1 => "fr",
        2 => "en",
        _ => "auto",
    }
}

fn buffer_text(b: &TextBuffer) -> String {
    b.text(&b.start_iter(), &b.end_iter(), false).to_string()
}

fn pad(w: &impl IsA<gtk4::Widget>, px: i32) {
    w.set_margin_top(px);
    w.set_margin_bottom(px);
    w.set_margin_start(px);
    w.set_margin_end(px);
}
