//! Liquid glass.
//!
//! Glass only looks like glass when you can see through it, so panels don't
//! paint themselves. The window's `GlassHost` draws the backdrop (the blurred
//! cover) through each one: slightly magnified like a lens, its colour lifted,
//! then a light tint and the edge lighting that gives the glass its thickness.
//!
//! The host holds the backdrop, the page content and one floating panel (the
//! player), which it stretches across a "lane" (the playlist column). Panels
//! inside the content, like the sidebar, are registered with `add_panel`.
//!
//! The Vinyl style turns the material off: panels then sit straight on the
//! backdrop, with only a soft shade behind them to keep text readable.

use std::cell::{Cell, OnceCell, RefCell};

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk};

/// The gap around floating glass, matching the sidebar panel's margins.
const GAP: i32 = 12;
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
        pub lane: glib::WeakRef<gtk::Widget>,
        pub panels: RefCell<Vec<glib::WeakRef<super::Glass>>>,
        pub shade_only: Cell<bool>,
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
                // Spans the lane (allocated by now, with the content): from
                // where the sidebar panel's gap ends, or the window edge when
                // there's no sidebar, to a gap short of the right edge.
                let lane_x = self
                    .lane
                    .upgrade()
                    .filter(|l| l.is_mapped())
                    .and_then(|l| l.compute_bounds(&*self.obj()))
                    .map_or(0, |b| b.x().round() as i32);
                let left = if lane_x > 0 { lane_x } else { GAP };
                let (min, _, _, _) = floating.measure(gtk::Orientation::Horizontal, -1);
                let w = (width - GAP - left).max(min);
                let (_, h, _, _) = floating.measure(gtk::Orientation::Vertical, w);
                let at = gsk::Transform::new().translate(&graphene::Point::new(left as f32, (height - h - GAP) as f32));
                floating.allocate(w, h, -1, Some(at));
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

            let shade_only = self.shade_only.get();
            let window = graphene::Rect::new(0.0, 0.0, obj.width() as f32, obj.height() as f32);
            self.panels.borrow_mut().retain(|p| p.upgrade().is_some());
            for panel in self.panels.borrow().iter().filter_map(|p| p.upgrade()) {
                if !panel.is_drawable() {
                    continue;
                }
                if let Some(bounds) = panel.compute_bounds(&*obj) {
                    if shade_only {
                        draw_side_shade(snapshot, &bounds, &window);
                    } else {
                        draw_material(snapshot, &bounds, panel.radius(), backdrop.as_ref());
                    }
                }
            }

            if let Some(content) = self.content.get() {
                obj.snapshot_child(content, snapshot);
            }

            if let Some(floating) = self.floating.get().filter(|f| f.is_drawable()) {
                if let Some(bounds) = floating.compute_bounds(&*obj) {
                    if shade_only {
                        draw_bottom_shade(snapshot, &bounds, &window);
                    } else {
                        draw_material(snapshot, &bounds, floating.radius(), backdrop.as_ref());
                    }
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

    /// The widget whose column the floating panel is centred in.
    pub fn set_lane(&self, lane: &impl IsA<gtk::Widget>) {
        self.imp().lane.set(Some(lane.upcast_ref()));
        self.queue_allocate();
    }

    /// A glass panel inside the content, with the backdrop showing through.
    pub fn add_panel(&self, panel: &Glass) {
        self.imp().panels.borrow_mut().push(panel.downgrade());
        self.queue_draw();
    }

    /// No glass, only shade behind the panels (the Vinyl style).
    pub fn set_shade_only(&self, on: bool) {
        self.imp().shade_only.set(on);
        self.queue_draw();
    }
}

/// Darkens from the window's left edge to just past the sidebar, so its text
/// reads on any cover without a panel around it.
fn draw_side_shade(snapshot: &gtk::Snapshot, panel: &graphene::Rect, window: &graphene::Rect) {
    let reach = panel.x() + panel.width() + 64.0;
    let area = graphene::Rect::new(0.0, 0.0, reach, window.height());
    let stop = |at: f32, alpha: f32| gsk::ColorStop::new(at / reach, black(alpha));
    snapshot.append_linear_gradient(
        &area,
        &graphene::Point::new(0.0, 0.0),
        &graphene::Point::new(reach, 0.0),
        &[stop(0.0, 0.42), stop(panel.x() + panel.width() * 0.6, 0.30), stop(reach, 0.0)],
    );
}

/// Darkens upwards from the window's bottom edge behind the player.
fn draw_bottom_shade(snapshot: &gtk::Snapshot, panel: &graphene::Rect, window: &graphene::Rect) {
    let top = (panel.y() - 72.0).max(0.0);
    let area = graphene::Rect::new(0.0, top, window.width(), window.height() - top);
    snapshot.append_linear_gradient(
        &area,
        &graphene::Point::new(0.0, top),
        &graphene::Point::new(0.0, window.height()),
        &[
            gsk::ColorStop::new(0.0, black(0.0)),
            gsk::ColorStop::new(0.5, black(0.38)),
            gsk::ColorStop::new(1.0, black(0.62)),
        ],
    );
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
fn draw_material(snapshot: &gtk::Snapshot, bounds: &graphene::Rect, radius: f32, behind: Option<&gsk::RenderNode>) {
    let (w, h) = (bounds.width(), bounds.height());
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let radius = radius.min(w / 2.0).min(h / 2.0);
    let shape = gsk::RoundedRect::from_rect(*bounds, radius);

    // Lift: one wide, soft shadow. (A tight contact shadow read as an outline.)
    snapshot.append_outset_shadow(&shape, &black(0.22), 0.0, 14.0, 0.0, 40.0);

    snapshot.push_rounded_clip(&shape);
    if let Some(behind) = behind {
        let (matrix, offset) = vibrancy();
        snapshot.push_color_matrix(&matrix, &offset);
        // Magnify around the panel's centre.
        let (cx, cy) = (bounds.x() + w / 2.0, bounds.y() + h / 2.0);
        snapshot.save();
        snapshot.translate(&graphene::Point::new(cx, cy));
        snapshot.scale(LENS, LENS);
        snapshot.translate(&graphene::Point::new(-cx, -cy));
        snapshot.append_node(behind);
        snapshot.restore();
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
            gsk::ColorStop::new(0.0, white(0.07)),
            gsk::ColorStop::new(0.5, white(0.02)),
            gsk::ColorStop::new(1.0, white(0.03)),
        ],
    );
    // Thickness without an outline: a soft glow inside the edge, brighter
    // towards the top, never a hard line.
    snapshot.append_inset_shadow(&shape, &white(0.035), 0.0, 0.0, 0.0, 18.0);
    snapshot.append_inset_shadow(&shape, &white(0.05), 0.0, 6.0, 0.0, 12.0);
    snapshot.pop();

    // Specular rim: light caught on the top-left corner and, faintly, the
    // bottom-right, gone along the straight edges so it never reads as a
    // border.
    snapshot.push_mask(gsk::MaskMode::Alpha);
    snapshot.append_border(&shape, &[1.0; 4], &[white(1.0); 4]);
    snapshot.pop();
    snapshot.append_linear_gradient(
        bounds,
        &graphene::Point::new(bounds.x(), bounds.y()),
        &graphene::Point::new(bounds.x() + w, bounds.y() + h),
        &[
            gsk::ColorStop::new(0.0, white(0.26)),
            gsk::ColorStop::new(0.2, white(0.04)),
            gsk::ColorStop::new(0.8, white(0.0)),
            gsk::ColorStop::new(1.0, white(0.10)),
        ],
    );
    snapshot.pop();
}
