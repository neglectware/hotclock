//! The renderer: the scene's zones drawn into an offscreen canvas, redrawing only a zone whose description
//! changed (damage-only redraw, 11.1). The damage list is what the KMS output copies into its buffers.

use crate::canvas::{Canvas, ZoneOps};
use crate::layout::{Layout, Plan, Rect};
use crate::scene::{self, Report, SceneCache, SceneInput};
use std::collections::HashMap;

pub struct Renderer {
    pub canvas: Canvas,
    pub plan: Plan,
    sigs: HashMap<String, u64>,
    layout_key: Option<Vec<(String, Rect)>>,
    cache: SceneCache,
    pub report: Report,
    pub layout: Option<Layout>,
    pub zones: Vec<ZoneOps>,
}

impl Renderer {
    pub fn new(plan: Plan) -> Renderer {
        Renderer {
            canvas: Canvas::new(plan.w as u32, plan.h as u32),
            plan,
            sigs: HashMap::new(),
            layout_key: None,
            cache: SceneCache::default(),
            report: Report::default(),
            layout: None,
            zones: vec![],
        }
    }

    /// Draw one frame. Returns the damaged rectangles (the whole screen when the layout changed).
    pub fn frame(&mut self, inp: &SceneInput) -> Vec<Rect> {
        self.canvas.log = Default::default();
        let (lay, zones, report) =
            scene::build(inp, self.canvas.fonts_for_scene(), &mut self.cache);
        let key: Vec<(String, Rect)> = zones.iter().map(|z| (z.name.clone(), z.rect)).collect();
        let mut damage = vec![];
        let full = self.layout_key.as_ref() != Some(&key);
        if full {
            self.sigs.clear();
            let screen = Rect::new(0, 0, self.canvas.width(), self.canvas.height());
            self.canvas
                .fill_rect(screen, crate::canvas::theme::BG, screen);
            damage.push(screen);
            self.layout_key = Some(key);
        }
        for z in &zones {
            let sig = z.signature();
            if self.sigs.get(&z.name) != Some(&sig) {
                // draw, and damage only the pixels that really changed: a progress bar that grew by a pixel, or a meter
                // bar that moved, is a few rows, not the whole zone (the copy to video memory is what costs)
                let before = if full {
                    None
                } else {
                    Some(self.canvas.copy_region(z.rect))
                };
                self.canvas.draw_zone(z);
                self.sigs.insert(z.name.clone(), sig);
                if let Some(b) = before {
                    if let Some(r) = self.canvas.changed_bbox(&b, z.rect) {
                        damage.push(r);
                    }
                }
            }
        }
        self.report = report;
        self.layout = Some(lay);
        self.zones = zones;
        damage
    }
}

impl Canvas {
    /// The fonts, for the scene's measuring (the scene never draws).
    pub fn fonts_for_scene(&self) -> &crate::canvas::Fonts {
        self.fonts()
    }
}
