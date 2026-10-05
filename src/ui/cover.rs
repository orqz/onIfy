//! A square cover image with rounded corners that fades in when it arrives.

use std::cell::{Cell, RefCell};

use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};

use crate::images;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Cover {
        pub size: Cell<i32>,
        pub radius: Cell<f32>,
        pub url: RefCell<Option<String>>,
        pub texture: RefCell<Option<gdk::Texture>>,
        pub opacity: Cell<f64>,
        pub fade: RefCell<Option<adw::TimedAnimation>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Cover {
        const NAME: &'static str = "OnifyCover";
        type Type = super::Cover;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("cover");
        }
    }

    impl ObjectImpl for Cover {}

    impl WidgetImpl for Cover {
        fn measure(&self, _: gtk::Orientation, _: i32) -> (i32, i32, i32, i32) {
            let size = self.size.get();
            (size, size, -1, -1)
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            // Always a centred square, however much room the parent gives.
            let widget = self.obj();
            let (w, h) = (widget.width() as f32, widget.height() as f32);
            let side = w.min(h);
            let bounds = graphene::Rect::new((w - side) / 2.0, (h - side) / 2.0, side, side);
            let radius = self.radius.get().min(bounds.width() / 2.0);
            snapshot.push_rounded_clip(&gsk::RoundedRect::from_rect(bounds, radius));
            let opacity = self.opacity.get();
            if opacity < 1.0 || self.texture.borrow().is_none() {
                snapshot.append_color(&gdk::RGBA::new(0.16, 0.16, 0.16, 1.0), &bounds);
            }
            if let Some(texture) = self.texture.borrow().as_ref() {
                snapshot.push_opacity(opacity);
                snapshot.append_scaled_texture(texture, gsk::ScalingFilter::Linear, &bounds);
                snapshot.pop();
            }
            snapshot.pop();
        }
    }
}

glib::wrapper! {
    pub struct Cover(ObjectSubclass<imp::Cover>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Cover {
    pub fn new(size: i32, radius: f32) -> Self {
        let cover: Self = glib::Object::new();
        cover.imp().size.set(size);
        cover.imp().radius.set(radius);
        cover.set_halign(gtk::Align::Start);
        cover.set_valign(gtk::Align::Center);
        cover
    }

    /// A circular cover, for artists.
    pub fn round(size: i32) -> Self {
        Self::new(size, size as f32 / 2.0)
    }

    pub fn set_url(&self, url: Option<&str>) {
        let imp = self.imp();
        if imp.url.borrow().as_deref() == url {
            return;
        }
        imp.url.replace(url.map(str::to_owned));
        if let Some(fade) = imp.fade.take() {
            fade.pause();
        }
        let Some(url) = url else {
            imp.texture.replace(None);
            self.queue_draw();
            return;
        };

        let px = (imp.size.get() * images::scale()) as u32;
        if let Some(texture) = images::cached(url, px) {
            imp.texture.replace(Some(texture));
            imp.opacity.set(1.0);
            self.queue_draw();
            return;
        }

        imp.texture.replace(None);
        imp.opacity.set(0.0);
        self.queue_draw();

        let want = url.to_owned();
        let weak = self.downgrade();
        let still_wanted = {
            let weak = weak.clone();
            let want = want.clone();
            move || {
                weak.upgrade()
                    .is_some_and(|c| c.imp().url.borrow().as_deref() == Some(want.as_str()))
            }
        };
        images::request(url, px, still_wanted, move |texture| {
            if let Some(cover) = weak.upgrade() {
                cover.fade_in(texture);
            }
        });
    }

    fn fade_in(&self, texture: &gdk::Texture) {
        let imp = self.imp();
        imp.texture.replace(Some(texture.clone()));
        if !self.is_mapped() {
            imp.opacity.set(1.0);
            self.queue_draw();
            return;
        }
        let weak = self.downgrade();
        let target = adw::CallbackAnimationTarget::new(move |value| {
            if let Some(cover) = weak.upgrade() {
                cover.imp().opacity.set(value);
                cover.queue_draw();
            }
        });
        let fade = adw::TimedAnimation::new(self, 0.0, 1.0, 180, target);
        fade.set_easing(adw::Easing::EaseOutCubic);
        fade.play();
        imp.fade.replace(Some(fade));
    }
}
