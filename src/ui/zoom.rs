//! Zoom: scales everything in the window, covers and buttons as much as text,
//! the way a browser (or Spotify's app) zooms. The child is laid out at the
//! window's size divided by the zoom and drawn scaled back up, so at 50% the
//! pages get twice the room. GTK maps the pointer through the same scale.

use std::cell::{Cell, RefCell};

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{glib, graphene, gsk};

mod imp {
    use super::*;

    pub struct Zoom {
        pub child: RefCell<Option<gtk::Widget>>,
        pub scale: Cell<f64>,
    }

    impl Default for Zoom {
        fn default() -> Self {
            Self { child: RefCell::default(), scale: Cell::new(1.0) }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Zoom {
        const NAME: &'static str = "OnifyZoom";
        type Type = super::Zoom;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Zoom {
        fn dispose(&self) {
            if let Some(child) = self.child.take() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for Zoom {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            self.child.borrow().as_ref().map_or(gtk::SizeRequestMode::ConstantSize, |c| c.request_mode())
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let Some(child) = self.child.borrow().clone() else { return (0, 0, -1, -1) };
            let scale = self.scale.get();
            let for_child = if for_size < 0 { -1 } else { (for_size as f64 / scale).ceil() as i32 };
            let (min, natural, _, _) = child.measure(orientation, for_child);
            ((min as f64 * scale).ceil() as i32, (natural as f64 * scale).ceil() as i32, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let Some(child) = self.child.borrow().clone() else { return };
            let scale = self.scale.get();
            // Rounded up, so the scaled child always reaches the edges.
            let (w, h) = ((width as f64 / scale).ceil() as i32, (height as f64 / scale).ceil() as i32);
            let transform = (scale != 1.0).then(|| gsk::Transform::new().scale(scale as f32, scale as f32));
            child.allocate(w, h, -1, transform);
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            // Rounding up overshoots by under a pixel; keep it in the window.
            snapshot.push_clip(&graphene::Rect::new(0.0, 0.0, obj.width() as f32, obj.height() as f32));
            if let Some(child) = self.child.borrow().as_ref() {
                obj.snapshot_child(child, snapshot);
            }
            snapshot.pop();
        }
    }
}

glib::wrapper! {
    pub struct Zoom(ObjectSubclass<imp::Zoom>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Zoom {
    pub fn new(child: &impl IsA<gtk::Widget>) -> Self {
        let zoom: Self = glib::Object::new();
        child.set_parent(&zoom);
        zoom.imp().child.replace(Some(child.clone().upcast()));
        zoom
    }

    pub fn set_scale(&self, scale: f64) {
        if self.imp().scale.replace(scale) != scale {
            self.queue_resize();
        }
    }
}
