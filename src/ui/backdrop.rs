//! The window background: the playing song's cover, blurred and stretched
//! edge to edge, cross-fading whenever the song changes. It also hands the
//! cover's accent colour to the rest of the UI.

use std::cell::{Cell, OnceCell, RefCell};

use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};

use crate::images::{self, BACKDROP, Rgb};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Backdrop {
        pub url: RefCell<Option<String>>,
        pub current: RefCell<Option<gdk::Texture>>,
        pub previous: RefCell<Option<gdk::Texture>>,
        pub mix: Cell<f64>,
        pub fade: RefCell<Option<adw::TimedAnimation>>,
        /// How much the cover is dimmed: see `Mood`.
        pub mood: Cell<super::Mood>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Backdrop {
        const NAME: &'static str = "OnifyBackdrop";
        type Type = super::Backdrop;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Backdrop {}

    impl WidgetImpl for Backdrop {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            let (w, h) = (widget.width() as f32, widget.height() as f32);
            let bounds = graphene::Rect::new(0.0, 0.0, w, h);
            snapshot.append_color(&gdk::RGBA::new(0.035, 0.035, 0.05, 1.0), &bounds);

            // Nothing playing yet: a calm two-tone glow instead of a cover.
            if self.current.borrow().is_none() {
                for (x, y, r, color) in [
                    (0.15, 0.1, 0.9, gdk::RGBA::new(0.32, 0.18, 0.62, 0.55)),
                    (0.9, 0.85, 0.8, gdk::RGBA::new(0.05, 0.38, 0.45, 0.45)),
                ] {
                    snapshot.append_radial_gradient(
                        &bounds,
                        &graphene::Point::new(w * x, h * y),
                        w.max(h) * r,
                        w.max(h) * r,
                        0.0,
                        1.0,
                        &[
                            gsk::ColorStop::new(0.0, color),
                            gsk::ColorStop::new(1.0, gdk::RGBA::new(color.red(), color.green(), color.blue(), 0.0)),
                        ],
                    );
                }
            }

            let draw = |texture: &gdk::Texture, alpha: f64| {
                if alpha <= 0.0 {
                    return;
                }
                // Fill the window, overscanned so the soft edges stay outside.
                let (tw, th) = (texture.width() as f32, texture.height() as f32);
                let scale = (w / tw).max(h / th) * 1.3;
                let (dw, dh) = (tw * scale, th * scale);
                let rect = graphene::Rect::new((w - dw) / 2.0, (h - dh) / 2.0, dw, dh);
                snapshot.push_opacity(alpha);
                snapshot.append_scaled_texture(texture, gsk::ScalingFilter::Linear, &rect);
                snapshot.pop();
            };
            let mix = self.mix.get();
            if let Some(previous) = self.previous.borrow().as_ref() {
                draw(previous, 1.0);
            }
            if let Some(current) = self.current.borrow().as_ref() {
                draw(current, mix);
            }

            // Deepen the colour, keep the top airy and the bottom grounded.
            let black = |a: f32| gdk::RGBA::new(0.02, 0.02, 0.04, a);
            let (top, middle, bottom, edge) = match self.mood.get() {
                super::Mood::Bright => (0.08, 0.18, 0.44, 0.26),
                super::Mood::Normal => (0.20, 0.34, 0.58, 0.38),
                super::Mood::Dark => (0.48, 0.60, 0.78, 0.50),
            };
            snapshot.append_linear_gradient(
                &bounds,
                &graphene::Point::new(0.0, 0.0),
                &graphene::Point::new(0.0, h),
                &[
                    gsk::ColorStop::new(0.0, black(top)),
                    gsk::ColorStop::new(0.5, black(middle)),
                    gsk::ColorStop::new(1.0, black(bottom)),
                ],
            );
            // Vignette.
            snapshot.append_radial_gradient(
                &bounds,
                &graphene::Point::new(w * 0.5, h * 0.4),
                w * 0.75,
                h * 0.9,
                0.0,
                1.0,
                &[
                    gsk::ColorStop::new(0.55, black(0.0)),
                    gsk::ColorStop::new(1.0, black(edge)),
                ],
            );
        }
    }
}

/// How dim the blurred cover behind everything is. Text stays white in all.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mood {
    /// The cover's colours glow.
    Bright,
    #[default]
    Normal,
    /// Deep and moody.
    Dark,
}

impl Mood {
    pub const ALL: [Mood; 3] = [Mood::Bright, Mood::Normal, Mood::Dark];

    pub fn name(self) -> &'static str {
        match self {
            Mood::Bright => "bright",
            Mood::Normal => "normal",
            Mood::Dark => "dark",
        }
    }

    pub fn from_name(name: &str) -> Self {
        Self::ALL.into_iter().find(|m| m.name() == name).unwrap_or_default()
    }
}

glib::wrapper! {
    pub struct Backdrop(ObjectSubclass<imp::Backdrop>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

thread_local! {
    static ACCENT_CSS: OnceCell<gtk::CssProvider> = const { OnceCell::new() };
}

impl Backdrop {
    pub fn new() -> Self {
        let backdrop: Self = glib::Object::new();
        backdrop.set_can_target(false);
        backdrop.set_hexpand(true);
        backdrop.set_vexpand(true);
        backdrop
    }

    pub fn set_mood(&self, mood: Mood) {
        self.imp().mood.set(mood);
        self.queue_draw();
    }

    pub fn set_cover(&self, url: Option<&str>) {
        let imp = self.imp();
        if imp.url.borrow().as_deref() == url {
            return;
        }
        imp.url.replace(url.map(str::to_owned));
        let Some(url) = url else { return };

        if let Some(texture) = images::cached(url, BACKDROP) {
            self.show(&texture, images::accent(url));
            return;
        }
        let want = url.to_owned();
        let weak = self.downgrade();
        let wanted = {
            let weak = weak.clone();
            let want = want.clone();
            move || weak.upgrade().is_some_and(|b| b.imp().url.borrow().as_deref() == Some(want.as_str()))
        };
        images::request(url, BACKDROP, wanted, move |texture| {
            if let Some(backdrop) = weak.upgrade() {
                backdrop.show(texture, images::accent(&want));
            }
        });
    }

    fn show(&self, texture: &gdk::Texture, accent: Option<Rgb>) {
        let imp = self.imp();
        if let Some(fade) = imp.fade.take() {
            fade.skip();
        }
        let old = imp.current.replace(Some(texture.clone()));
        imp.previous.replace(old);
        imp.mix.set(0.0);
        if let Some(accent) = accent {
            set_accent(accent);
        }

        let weak = self.downgrade();
        let target = adw::CallbackAnimationTarget::new(move |value| {
            if let Some(backdrop) = weak.upgrade() {
                backdrop.imp().mix.set(value);
                backdrop.queue_draw();
            }
        });
        let fade = adw::TimedAnimation::new(self, 0.0, 1.0, 900, target);
        fade.set_easing(adw::Easing::EaseInOutCubic);
        let weak = self.downgrade();
        fade.connect_done(move |_| {
            if let Some(backdrop) = weak.upgrade() {
                // The old cover is fully hidden now; stop drawing it.
                backdrop.imp().previous.replace(None);
            }
        });
        fade.play();
        imp.fade.replace(Some(fade));
    }
}

/// Recolours accents (progress bar, playing row, toggles) to match the cover.
fn set_accent([r, g, b]: Rgb) {
    let hex = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
    let color = format!("#{:02x}{:02x}{:02x}", hex(r), hex(g), hex(b));
    ACCENT_CSS.with(|css| {
        let provider = css.get_or_init(|| {
            let provider = gtk::CssProvider::new();
            if let Some(display) = gdk::Display::default() {
                gtk::style_context_add_provider_for_display(
                    &display,
                    &provider,
                    gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
                );
            }
            provider
        });
        provider.load_from_string(&format!(
            ":root {{ --accent-color: {color}; --accent-bg-color: {color}; --accent-fg-color: #101010; }}"
        ));
    });
}
