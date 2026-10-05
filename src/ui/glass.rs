//! Liquid glass.
//!
//! Glass only looks like glass when you can see through it, so panels don't
//! paint themselves. The window's `GlassHost` knows what lies behind each one
//! (the blurred cover and the page underneath) and draws it through the panel:
//! blurred, slightly magnified like a lens, with its colour lifted, then a
//! light tint and the edge lighting that gives the glass its thickness.
//!
//! The host holds the backdrop, the page content and one floating panel (the
//! player). Panels inside the content, like the sidebar, are registered with
//! `add_panel` and see the backdrop through them.

use std::cell::{Cell, OnceCell, RefCell};

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};

/// Blur for what's seen through floating glass.
const BLUR: f32 = 26.0;
/// Glass magnifies what's behind it a touch, like a thick lens.
const LENS: f32 = 1.035;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Glass {
        pub radius: Cell<f32>,
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
    impl WidgetImpl for Glass {}

    #[derive(Default)]
    pub struct GlassHost {
        pub backdrop: OnceCell<gtk::Widget>,
        pub content: OnceCell<gtk::Widget>,
        pub floating: OnceCell<super::Glass>,
        pub panels: RefCell<Vec<glib::WeakRef<super::Glass>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for GlassHost {
        const NAME: &'static str = "OnifyGlassHost";
        type Type = super::GlassHost;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for GlassHost {
        fn dispose(&self) {
            let obj = self.obj();
            while let Some(child) = obj.first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for GlassHost {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let (mut min, mut nat) = (0, 0);
            if let Some(content) = self.content.get() {
                (min, nat, _, _) = content.measure(orientation, for_size);
            }
            if let Some(floating) = self.floating.get().filter(|f| f.is_visible()) {
                let (fmin, fnat, _, _) = floating.measure(orientation, -1);
                min = min.max(fmin);
                nat = nat.max(fnat);
            }
            (min, nat, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            for child in [self.backdrop.get(), self.content.get()].into_iter().flatten() {
                child.allocate(width, height, baseline, None);
            }
            if let Some(floating) = self.floating.get().filter(|f| f.is_visible()) {
                // Full width, resting on the bottom edge; its CSS margins keep
                // it off the window edges.
                let (_, h, _, _) = floating.measure(gtk::Orientation::Vertical, width);
                let at = gsk::Transform::new().translate(&graphene::Point::new(0.0, (height - h) as f32));
                floating.allocate(width, h, -1, Some(at));
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let node_of = |child: &gtk::Widget| {
                let s = gtk::Snapshot::new();
                obj.snapshot_child(child, &s);
                s.to_node()
            };
            let backdrop = self.backdrop.get().and_then(node_of);
            if let Some(node) = &backdrop {
                snapshot.append_node(node);
            }

            // Panels in the content see only the backdrop, which is already
            // soft, so they skip the blur.
            self.panels.borrow_mut().retain(|p| p.upgrade().is_some());
            for panel in self.panels.borrow().iter().filter_map(|p| p.upgrade()) {
                if !panel.is_drawable() {
                    continue;
                }
                if let Some(bounds) = panel.compute_bounds(&*obj) {
                    draw_material(snapshot, &bounds, panel.radius(), backdrop.as_ref(), 0.0);
                }
            }

            let content = self.content.get().and_then(node_of);
            if let Some(node) = &content {
                snapshot.append_node(node);
            }

            if let Some(floating) = self.floating.get().filter(|f| f.is_drawable()) {
                if let Some(bounds) = floating.compute_bounds(&*obj) {
                    let behind: Vec<gsk::RenderNode> = [backdrop, content].into_iter().flatten().collect();
                    let behind: gsk::RenderNode = gsk::ContainerNode::new(&behind).upcast();
                    draw_material(snapshot, &bounds, floating.radius(), Some(&behind), BLUR);
                }
                obj.snapshot_child(floating, snapshot);
            }
        }
    }
}

glib::wrapper! {
    /// A box that sits on glass. It draws nothing itself; the host does.
    pub struct Glass(ObjectSubclass<imp::Glass>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl Glass {
    pub fn new(orientation: gtk::Orientation, spacing: i32, radius: f32) -> Self {
        let glass: Self = glib::Object::builder()
            .property("orientation", orientation)
            .property("spacing", spacing)
            .build();
        glass.imp().radius.set(radius);
        glass
    }

    pub fn radius(&self) -> f32 {
        self.imp().radius.get()
    }
}

glib::wrapper! {
    pub struct GlassHost(ObjectSubclass<imp::GlassHost>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl GlassHost {
    pub fn new(backdrop: &impl IsA<gtk::Widget>, content: &impl IsA<gtk::Widget>, floating: &Glass) -> Self {
        let host: Self = glib::Object::new();
        let imp = host.imp();
        backdrop.set_parent(&host);
        content.set_parent(&host);
        floating.set_parent(&host);
        let _ = imp.backdrop.set(backdrop.clone().upcast());
        let _ = imp.content.set(content.clone().upcast());
        let _ = imp.floating.set(floating.clone());
        host
    }

    /// A glass panel inside the content, with the backdrop showing through.
    pub fn add_panel(&self, panel: &Glass) {
        self.imp().panels.borrow_mut().push(panel.downgrade());
        self.queue_draw();
    }
}

fn white(alpha: f32) -> gdk::RGBA {
    gdk::RGBA::new(1.0, 1.0, 1.0, alpha)
}

fn black(alpha: f32) -> gdk::RGBA {
    gdk::RGBA::new(0.0, 0.0, 0.02, alpha)
}

/// Lifts saturation and brightness of what's seen through the glass. Equal
/// channel weights keep the matrix symmetric, so row/column order can't flip it.
fn vibrancy() -> (graphene::Matrix, graphene::Vec4) {
    const SATURATION: f32 = 1.45;
    const BRIGHTNESS: f32 = 1.06;
    let d = BRIGHTNESS * (SATURATION + (1.0 - SATURATION) / 3.0);
    let o = BRIGHTNESS * (1.0 - SATURATION) / 3.0;
    let matrix = graphene::Matrix::from_float([
        d, o, o, 0.0, //
        o, d, o, 0.0, //
        o, o, d, 0.0, //
        0.0, 0.0, 0.0, 1.0,
    ]);
    (matrix, graphene::Vec4::new(0.0, 0.0, 0.0, 0.0))
}

/// Draws a glass panel's material at `bounds`, seeing `behind` through it.
fn draw_material(snapshot: &gtk::Snapshot, bounds: &graphene::Rect, radius: f32, behind: Option<&gsk::RenderNode>, blur: f32) {
    let (w, h) = (bounds.width(), bounds.height());
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let radius = radius.min(w / 2.0).min(h / 2.0);
    let shape = gsk::RoundedRect::from_rect(*bounds, radius);

    // Lift: a wide soft shadow and a tight contact one.
    snapshot.append_outset_shadow(&shape, &black(0.28), 0.0, 16.0, 0.0, 44.0);
    snapshot.append_outset_shadow(&shape, &black(0.20), 0.0, 1.0, 0.0, 3.0);

    snapshot.push_rounded_clip(&shape);
    if let Some(behind) = behind {
        let (matrix, offset) = vibrancy();
        snapshot.push_color_matrix(&matrix, &offset);
        if blur > 0.0 {
            snapshot.push_blur(blur as f64);
        }
        // Magnify around the panel's centre, sampling only the area the blur
        // can reach so the offscreen stays panel-sized.
        let (cx, cy) = (bounds.x() + w / 2.0, bounds.y() + h / 2.0);
        snapshot.save();
        snapshot.translate(&graphene::Point::new(cx, cy));
        snapshot.scale(LENS, LENS);
        snapshot.translate(&graphene::Point::new(-cx, -cy));
        let reach = blur * 2.0;
        snapshot.push_clip(&bounds.inset_r(-reach, -reach));
        snapshot.append_node(behind);
        snapshot.pop();
        snapshot.restore();
        if blur > 0.0 {
            snapshot.pop();
        }
        snapshot.pop();
    }
    // A smoky tint keeps white text readable over bright covers, and a soft
    // light from above gives the body depth.
    snapshot.append_color(&gdk::RGBA::new(0.06, 0.06, 0.09, 0.26), bounds);
    snapshot.append_linear_gradient(
        bounds,
        &graphene::Point::new(bounds.x(), bounds.y()),
        &graphene::Point::new(bounds.x(), bounds.y() + h),
        &[
            gsk::ColorStop::new(0.0, white(0.10)),
            gsk::ColorStop::new(0.55, white(0.03)),
            gsk::ColorStop::new(1.0, white(0.05)),
        ],
    );
    // Thickness: light caught along the inside of the top edge, a faint glow
    // all round, and a little shade along the bottom.
    snapshot.append_inset_shadow(&shape, &white(0.20), 0.0, 1.0, 0.0, 1.0);
    snapshot.append_inset_shadow(&shape, &white(0.06), 0.0, 0.0, 0.0, 14.0);
    snapshot.append_inset_shadow(&shape, &black(0.16), 0.0, -1.0, 0.0, 2.0);
    snapshot.pop();

    // Specular rim: bright where the light hits (top left), clear along the
    // sides, picking up again at the bottom right as it refracts through.
    snapshot.push_mask(gsk::MaskMode::Alpha);
    snapshot.append_border(&shape, &[1.0; 4], &[white(1.0); 4]);
    snapshot.pop();
    snapshot.append_linear_gradient(
        bounds,
        &graphene::Point::new(bounds.x(), bounds.y()),
        &graphene::Point::new(bounds.x() + w, bounds.y() + h),
        &[
            gsk::ColorStop::new(0.0, white(0.55)),
            gsk::ColorStop::new(0.35, white(0.10)),
            gsk::ColorStop::new(0.7, white(0.06)),
            gsk::ColorStop::new(1.0, white(0.32)),
        ],
    );
    snapshot.pop();
}
