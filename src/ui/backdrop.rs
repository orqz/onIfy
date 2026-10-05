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
            snapshot.append_linear_gradient(
                &bounds,
                &graphene::Point::new(0.0, 0.0),
                &graphene::Point::new(0.0, h),
                &[
                    gsk::ColorStop::new(0.0, black(0.30)),
                    gsk::ColorStop::new(0.5, black(0.48)),
                    gsk::ColorStop::new(1.0, black(0.72)),
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
                    gsk::ColorStop::new(1.0, black(0.45)),
                ],
            );
            // Fine grain keeps the huge soft gradients from banding.
            let grain = grain();
            let tile = graphene::Rect::new(0.0, 0.0, grain.width() as f32 / 2.0, grain.height() as f32 / 2.0);
            snapshot.push_repeat(&bounds, Some(&tile));
            snapshot.append_texture(&grain, &tile);
            snapshot.pop();
        }
    }
}

thread_local! {
    static GRAIN: OnceCell<gdk::Texture> = const { OnceCell::new() };
}

fn grain() -> gdk::Texture {
    GRAIN.with(|g| {
        g.get_or_init(|| {
            const SIZE: usize = 128;
            let mut seed: u32 = 0x9e37_79b9;
            let mut pixels = Vec::with_capacity(SIZE * SIZE * 4);
            for _ in 0..SIZE * SIZE {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let alpha = (seed % 9) as u8; // up to ~3.5% opacity
                let v = if seed & 0x100 == 0 { 255 } else { 0 };
                // Premultiplied.
                let c = (v as u32 * alpha as u32 / 255) as u8;
                pixels.extend_from_slice(&[c, c, c, alpha]);
            }
            gdk::MemoryTexture::new(
                SIZE as i32,
                SIZE as i32,
                gdk::MemoryFormat::R8g8b8a8Premultiplied,
                &glib::Bytes::from_owned(pixels),
                SIZE * 4,
            )
            .upcast()
        })
        .clone()
    })
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
