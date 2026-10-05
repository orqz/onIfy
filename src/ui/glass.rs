//! Liquid glass: a translucent rounded sheet with a soft shadow, a specular
//! sheen and a gradient rim that catches the light at the top edge. It is
//! drawn behind the box's children, so any layout can sit on glass.

use std::cell::Cell;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Glass {
        pub radius: Cell<f32>,
        pub shadow: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Glass {
        const NAME: &'static str = "OnifyGlass";
        type Type = super::Glass;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("glass");
        }
    }

    impl ObjectImpl for Glass {}
    impl BoxImpl for Glass {}

    impl WidgetImpl for Glass {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            let (w, h) = (widget.width() as f32, widget.height() as f32);
            if w > 0.0 && h > 0.0 {
                draw_glass(snapshot, w, h, self.radius.get(), self.shadow.get());
            }
            self.parent_snapshot(snapshot);
        }
    }
}

glib::wrapper! {
    pub struct Glass(ObjectSubclass<imp::Glass>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

fn white(alpha: f32) -> gdk::RGBA {
    gdk::RGBA::new(1.0, 1.0, 1.0, alpha)
}

pub fn draw_glass(snapshot: &gtk::Snapshot, w: f32, h: f32, radius: f32, shadow: bool) {
    let rect = graphene::Rect::new(0.0, 0.0, w, h);
    let radius = radius.min(w / 2.0).min(h / 2.0);
    let shape = gsk::RoundedRect::from_rect(rect, radius);

    if shadow {
        snapshot.append_outset_shadow(&shape, &gdk::RGBA::new(0.0, 0.0, 0.0, 0.34), 0.0, 18.0, 0.0, 44.0);
    }

    snapshot.push_rounded_clip(&shape);
    // Body: a faint frost that is lighter at the top.
    snapshot.append_linear_gradient(
        &rect,
        &graphene::Point::new(0.0, 0.0),
        &graphene::Point::new(0.0, h),
        &[gsk::ColorStop::new(0.0, white(0.17)), gsk::ColorStop::new(1.0, white(0.075))],
    );
    // Specular sheen, as if lit from the top left.
    snapshot.append_radial_gradient(
        &rect,
        &graphene::Point::new(w * 0.22, -h * 0.35),
        (w * 0.55).max(h),
        h * 1.3,
        0.0,
        1.0,
        &[gsk::ColorStop::new(0.0, white(0.16)), gsk::ColorStop::new(1.0, white(0.0))],
    );
    snapshot.pop();

    // Rim: a 1px ring filled with a gradient, bright on top, dim at the sides,
    // picking up again along the bottom edge like refracted light.
    snapshot.push_mask(gsk::MaskMode::Alpha);
    snapshot.append_border(&shape, &[1.0; 4], &[white(1.0); 4]);
    snapshot.pop();
    snapshot.append_linear_gradient(
        &rect,
        &graphene::Point::new(0.0, 0.0),
        &graphene::Point::new(w * 0.3, h),
        &[
            gsk::ColorStop::new(0.0, white(0.62)),
            gsk::ColorStop::new(0.45, white(0.10)),
            gsk::ColorStop::new(1.0, white(0.30)),
        ],
    );
    snapshot.pop();
}

impl Glass {
    pub fn new(orientation: gtk::Orientation, spacing: i32, radius: f32) -> Self {
        let glass: Self = glib::Object::builder()
            .property("orientation", orientation)
            .property("spacing", spacing)
            .build();
        glass.imp().radius.set(radius);
        glass.imp().shadow.set(true);
        glass
    }

    pub fn set_shadow(&self, shadow: bool) {
        self.imp().shadow.set(shadow);
        self.queue_draw();
    }
}
