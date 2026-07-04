//! One open board: model + view state + selection + the canvas painting
//! and interaction. Net expansion ("follow the rail through jumpers") lives
//! here too.

use crate::view::View;
use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use fbv_core::{netclass, BoardModel, NetId, Point, Side, NO_NET};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CenterRequest {
    Part(usize),
    Net(usize),
}

/// Drawable outline for one part, precomputed in world space.
/// Two-pin chip parts (R/C/L/...) get a box oriented along their pin axis,
/// like OpenBoardView; everything else gets an expanded bounding box.
#[derive(Debug, Clone, Copy)]
pub struct PartOutline {
    pub corners: [Point; 4],
    pub center: Point,
}

/// Computes outlines for every part (None for dummies/empty parts).
pub fn compute_part_outlines(model: &BoardModel) -> Vec<Option<PartOutline>> {
    model
        .parts
        .iter()
        .enumerate()
        .map(|(idx, part)| {
            if part.is_dummy || part.pins.is_empty() {
                return None;
            }
            let pins: Vec<&fbv_core::Pin> = part
                .pins
                .iter()
                .map(|&pi| &model.pins[pi as usize])
                .collect();
            let max_r = pins.iter().fold(2.0f32, |a, p| a.max(p.radius));
            let margin = (max_r * 1.4).max(4.0);

            if pins.len() == 2 {
                let (a, b) = (pins[0].pos, pins[1].pos);
                let dx = b.x - a.x;
                let dy = b.y - a.y;
                let len = (dx * dx + dy * dy).sqrt();
                if len > 0.01 {
                    let (ux, uy) = (dx / len, dy / len);
                    let (vx, vy) = (-uy, ux);
                    let cx = (a.x + b.x) / 2.0;
                    let cy = (a.y + b.y) / 2.0;
                    let hl = len / 2.0 + margin;
                    let hw = max_r + margin * 0.6;
                    let corner = |su: f32, sv: f32| {
                        Point::new(cx + su * ux * hl + sv * vx * hw, cy + su * uy * hl + sv * vy * hw)
                    };
                    return Some(PartOutline {
                        corners: [corner(-1.0, -1.0), corner(1.0, -1.0), corner(1.0, 1.0), corner(-1.0, 1.0)],
                        center: Point::new(cx, cy),
                    });
                }
            }

            let (min, max) = model.part_bounds(idx)?;
            let min = Point::new(min.x - margin, min.y - margin);
            let max = Point::new(max.x + margin, max.y + margin);
            Some(PartOutline {
                corners: [
                    Point::new(min.x, min.y),
                    Point::new(max.x, min.y),
                    Point::new(max.x, max.y),
                    Point::new(min.x, max.y),
                ],
                center: Point::new((min.x + max.x) / 2.0, (min.y + max.y) / 2.0),
            })
        })
        .collect()
}

pub struct BoardTab {
    pub board_id: i64,
    pub title: String,
    pub model: Arc<BoardModel>,
    pub outlines: Vec<Option<PartOutline>>,
    pub view: View,
    pub selected_part: Option<usize>,
    /// The exact pin last clicked — origin of the netweb fan.
    pub selected_pin: Option<usize>,
    pub selected_net: Option<NetId>,
    /// 0 = off; 1..=3 jumper-expansion levels.
    pub expansion: u8,
    /// Show the far side's parts as faint ghosts under the facing side.
    /// Off by default: the facing side shows only its own components.
    pub ghost_back: bool,
    /// FlexBV-style "netweb": lines from the selected pin to every pad on
    /// the selected net.
    pub show_netweb: bool,
    pub center_request: Option<CenterRequest>,
    pub flash_until: f64,
    pub inboard_query: String,
    pub focus_search: bool,
    pub hover_world: Option<Point>,
    pub harvested: HashSet<i64>,
}

impl BoardTab {
    pub fn new(board_id: i64, title: String, model: Arc<BoardModel>) -> Self {
        let mut view = View::default();
        if let Some((min, max)) = model.bounds() {
            view.pivot = Point::new((min.x + max.x) / 2.0, (min.y + max.y) / 2.0);
        }
        let outlines = compute_part_outlines(&model);
        Self {
            board_id,
            title,
            model,
            outlines,
            view,
            selected_part: None,
            selected_pin: None,
            selected_net: None,
            expansion: 0,
            ghost_back: false,
            show_netweb: true,
            center_request: None,
            flash_until: 0.0,
            inboard_query: String::new(),
            focus_search: false,
            hover_world: None,
            harvested: HashSet::new(),
        }
    }

    /// Which sides are "facing the viewer" right now.
    fn facing(&self) -> Side {
        if self.view.bottom {
            Side::Bottom
        } else {
            Side::Top
        }
    }

    pub fn select_pin(&mut self, pin_idx: usize) {
        let pin = &self.model.pins[pin_idx];
        self.selected_part = Some(pin.part as usize);
        self.selected_pin = Some(pin_idx);
        self.selected_net = (pin.net != NO_NET).then_some(pin.net);
    }

    /// Pin the netweb fan radiates from: the clicked pin when it belongs to
    /// the selected net, otherwise a facing-side member of the net.
    fn netweb_origin(&self) -> Option<usize> {
        let net = self.selected_net?;
        if let Some(pi) = self.selected_pin {
            if self.model.pins[pi].net == net {
                return Some(pi);
            }
        }
        let facing = self.facing();
        let members = &self.model.nets[net as usize].pins;
        members
            .iter()
            .copied()
            .find(|&pi| {
                let s = self.model.pins[pi as usize].side;
                s == facing || s == Side::Both
            })
            .or_else(|| members.first().copied())
            .map(|v| v as usize)
    }

    pub fn select_net(&mut self, net: NetId) {
        self.selected_net = Some(net);
    }

    /// Net -> highlight level for the current selection (0 = the net
    /// itself, 1..=3 reached through jumper parts).
    pub fn highlight_map(&self) -> HashMap<NetId, u8> {
        let mut map = HashMap::new();
        let Some(root) = self.selected_net else {
            return map;
        };
        map.insert(root, 0u8);
        if self.expansion == 0 {
            return map;
        }
        let mut frontier = vec![root];
        for level in 1..=self.expansion {
            let mut next = Vec::new();
            for &net in &frontier {
                for &pin_idx in &self.model.nets[net as usize].pins {
                    let pin = &self.model.pins[pin_idx as usize];
                    let part = &self.model.parts[pin.part as usize];
                    if part.is_dummy
                        || part.pins.len() != 2
                        || !netclass::is_jumper_refdes(&part.refdes)
                    {
                        continue;
                    }
                    for &other_idx in &part.pins {
                        let other = &self.model.pins[other_idx as usize];
                        if other.net == NO_NET || other.net == net {
                            continue;
                        }
                        let name = &self.model.nets[other.net as usize].name;
                        if netclass::is_ground(name) {
                            continue;
                        }
                        if !map.contains_key(&other.net) {
                            map.insert(other.net, level);
                            next.push(other.net);
                        }
                    }
                }
            }
            frontier = next;
            if frontier.is_empty() {
                break;
            }
        }
        map
    }

    /// Applies a pending center request once the viewport size is known.
    fn apply_center_request(&mut self, viewport: Rect, now: f64) {
        let Some(req) = self.center_request.take() else {
            return;
        };
        let bounds = match req {
            CenterRequest::Part(idx) => {
                if let Some(part) = self.model.parts.get(idx) {
                    if part.side == Side::Bottom {
                        self.view.bottom = true;
                    } else if part.side == Side::Top {
                        self.view.bottom = false;
                    }
                    self.selected_part = Some(idx);
                    // Selecting the first pin's net makes the halo and the
                    // netweb work out of the box.
                    self.selected_pin = part.pins.first().map(|&pi| pi as usize);
                    self.selected_net = part
                        .pins
                        .first()
                        .map(|&pi| self.model.pins[pi as usize].net)
                        .filter(|&n| n != NO_NET);
                }
                self.model.part_bounds(idx)
            }
            CenterRequest::Net(idx) => {
                self.selected_net = Some(idx as NetId);
                self.selected_part = None;
                self.model.net_bounds(idx as NetId)
            }
        };
        if let Some((min, max)) = bounds {
            let margin = 200.0f32.max((max.x - min.x).max(max.y - min.y) * 0.5);
            self.view.fit(
                Point::new(min.x - margin, min.y - margin),
                Point::new(max.x + margin, max.y + margin),
                viewport,
            );
        }
        self.flash_until = now + 1.5;
    }

    /// Paints the board and handles canvas interaction.
    pub fn canvas(&mut self, ui: &mut egui::Ui) {
        let size = ui.available_size();
        let (response, painter) = ui.allocate_painter(size, Sense::click_and_drag());
        let viewport = response.rect;
        let now = ui.input(|i| i.time);

        painter.rect_filled(viewport, 0.0, Color32::from_rgb(16, 18, 22));

        if self.view.fit_pending {
            if let Some((min, max)) = self.model.bounds() {
                self.view.fit(min, max, viewport);
            } else {
                self.view.fit_pending = false;
            }
        }
        self.apply_center_request(viewport, now);

        // --- interaction ---
        if response.dragged() {
            self.view.pan_screen(response.drag_delta());
        }
        if let Some(hover) = response.hover_pos() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.0 {
                let factor = (scroll * 0.0035).exp();
                self.view.zoom_at(hover, viewport, factor);
            }
            self.hover_world = Some(self.view.world_of_screen(hover, viewport));
        } else {
            self.hover_world = None;
        }

        let highlight = self.highlight_map();
        let facing = self.facing();

        // --- board outline ---
        let outline_stroke = Stroke::new(1.0, Color32::from_rgb(90, 160, 90));
        for (a, b) in &self.model.outline {
            let pa = self.view.to_screen(*a, viewport);
            let pb = self.view.to_screen(*b, viewport);
            if seg_maybe_visible(pa, pb, viewport) {
                painter.line_segment([pa, pb], outline_stroke);
            }
        }

        // --- part outlines (ghosted far side first, facing side on top) ---
        const PART_COLOR: Color32 = Color32::from_rgb(120, 140, 170);
        let selected_stroke = Stroke::new(2.0, Color32::from_rgb(255, 130, 60));
        for pass in [false, true] {
            // pass false = far side ghosts, pass true = facing side
            for (idx, outline) in self.outlines.iter().enumerate() {
                let Some(outline) = outline else { continue };
                let part = &self.model.parts[idx];
                let part_faces = part.side == facing || part.side == Side::Both;
                if part_faces != pass {
                    continue;
                }
                if !pass && !self.ghost_back && self.selected_part != Some(idx) {
                    continue;
                }
                let pts: Vec<Pos2> = outline
                    .corners
                    .iter()
                    .map(|&c| self.view.to_screen(c, viewport))
                    .collect();
                let bb = Rect::from_points(&pts);
                if !viewport.intersects(bb) {
                    continue;
                }
                let stroke = if self.selected_part == Some(idx) {
                    selected_stroke
                } else if pass {
                    Stroke::new(1.0, PART_COLOR)
                } else {
                    Stroke::new(1.0, PART_COLOR.gamma_multiply(0.22))
                };
                painter.add(egui::Shape::closed_line(pts, stroke));
            }
        }

        // --- pins ---
        let mut hover_pin: Option<(usize, f32)> = None;
        let hover_pos = response.hover_pos();
        for (i, pin) in self.model.pins.iter().enumerate() {
            let pos = self.view.to_screen(pin.pos, viewport);
            let r = self.view.px(pin.radius.max(2.0));
            if !viewport.expand(r + 12.0).contains(pos) {
                continue;
            }

            let level = (pin.net != NO_NET)
                .then(|| highlight.get(&pin.net).copied())
                .flatten();
            let on_facing_side = pin.side == facing || pin.side == Side::Both;
            let part_selected = self.selected_part == Some(pin.part as usize);
            let harvested = self.harvested.contains(&(pin.part as i64));

            // With ghosting off, the far side is fully hidden (except
            // highlighted nets and the selected part).
            if !on_facing_side && !self.ghost_back && level.is_none() && !part_selected {
                continue;
            }

            // Halo behind highlighted nets, visible on both sides (dimmer
            // on the far side so the facing side stays dominant).
            if let Some(level) = level {
                let mut halo = level_color(level).gamma_multiply(0.35);
                if !on_facing_side {
                    halo = halo.gamma_multiply(0.5);
                }
                painter.circle_filled(pos, r * 2.0 + 2.0, halo);
            }

            let mut color = if let Some(level) = level {
                level_color(level)
            } else if pin.is_test_pad {
                Color32::from_rgb(70, 140, 100)
            } else if pin.net == NO_NET {
                Color32::from_gray(55)
            } else {
                Color32::from_rgb(120, 150, 190)
            };
            if !on_facing_side {
                // Far-side pads: highlighted nets stay traceable at half
                // strength, everything else is a faint ghost.
                color = color.gamma_multiply(if level.is_some() { 0.5 } else { 0.30 });
            }
            if part_selected {
                color = Color32::from_rgb(240, 80, 80);
            }
            if harvested {
                color = color.gamma_multiply(0.4);
            }
            painter.circle_filled(pos, r, color);
            if harvested {
                let d = r.max(3.0);
                let s = Stroke::new(1.5, Color32::from_rgb(200, 90, 60));
                painter.line_segment([pos + Vec2::new(-d, -d), pos + Vec2::new(d, d)], s);
                painter.line_segment([pos + Vec2::new(-d, d), pos + Vec2::new(d, -d)], s);
            }

            if let Some(h) = hover_pos {
                let dist = pos.distance(h);
                if dist < (r + 6.0).max(9.0) && hover_pin.map(|(_, d)| dist < d).unwrap_or(true) {
                    hover_pin = Some((i, dist));
                }
            }
        }

        // --- netweb: lines from the selected pin to every pad on the net,
        // FlexBV style, so the rail's reach is visible at a glance ---
        if self.show_netweb {
            if let (Some(net), Some(origin)) = (self.selected_net, self.netweb_origin()) {
                let o = self.view.to_screen(self.model.pins[origin].pos, viewport);
                let near = Stroke::new(1.2, Color32::from_rgba_unmultiplied(255, 220, 60, 140));
                let far = Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 220, 60, 55));
                for &pi in &self.model.nets[net as usize].pins {
                    let pi = pi as usize;
                    if pi == origin {
                        continue;
                    }
                    let pin = &self.model.pins[pi];
                    let p = self.view.to_screen(pin.pos, viewport);
                    if !seg_maybe_visible(o, p, viewport) {
                        continue;
                    }
                    let on_face = pin.side == facing || pin.side == Side::Both;
                    painter.line_segment([o, p], if on_face { near } else { far });
                }
                // Mark the origin so the fan's root is obvious.
                painter.circle_stroke(
                    o,
                    6.0,
                    Stroke::new(1.5, Color32::from_rgb(255, 220, 60)),
                );
            }
        }

        // --- refdes labels, centered in each part once it is readable ---
        for (idx, outline) in self.outlines.iter().enumerate() {
            let Some(outline) = outline else { continue };
            let part = &self.model.parts[idx];
            let part_faces = part.side == facing || part.side == Side::Both;
            let is_selected = self.selected_part == Some(idx);
            if !part_faces && !is_selected {
                continue;
            }
            let pts: [Pos2; 4] = [
                self.view.to_screen(outline.corners[0], viewport),
                self.view.to_screen(outline.corners[1], viewport),
                self.view.to_screen(outline.corners[2], viewport),
                self.view.to_screen(outline.corners[3], viewport),
            ];
            let bb = Rect::from_points(&pts);
            if !viewport.intersects(bb) {
                continue;
            }
            let (w, h) = (bb.width(), bb.height());
            // Big enough on screen to carry a readable label?
            if !is_selected && (w.max(h) < 26.0 || w.min(h) < 10.0) {
                continue;
            }
            let font_px = (w.min(h) * 0.42).clamp(9.0, 15.0);
            let pos = self.view.to_screen(outline.center, viewport);
            let color = if is_selected {
                Color32::from_rgb(255, 200, 120)
            } else {
                Color32::from_gray(215)
            };
            painter.text(
                pos,
                Align2::CENTER_CENTER,
                &part.refdes,
                FontId::proportional(font_px),
                color,
            );
        }

        // --- selection flash ---
        if now < self.flash_until {
            if let Some(idx) = self.selected_part {
                if let Some((min, max)) = self.model.part_bounds(idx) {
                    let center = Point::new((min.x + max.x) / 2.0, (min.y + max.y) / 2.0);
                    let pos = self.view.to_screen(center, viewport);
                    let phase = ((self.flash_until - now) * 6.0).sin().abs() as f32;
                    painter.circle_stroke(
                        pos,
                        18.0 + phase * 14.0,
                        Stroke::new(2.5, Color32::from_rgb(255, 220, 60)),
                    );
                }
            }
            ui.ctx().request_repaint();
        }

        // --- hover tooltip + click ---
        if let Some((pin_idx, _)) = hover_pin {
            let pin = &self.model.pins[pin_idx];
            let part = &self.model.parts[pin.part as usize];
            let net = if pin.net == NO_NET {
                "(no net)".to_string()
            } else {
                self.model.nets[pin.net as usize].name.clone()
            };
            let label = if part.is_dummy {
                format!("test pad {} · {}", pin.number, net)
            } else if pin.number.is_empty() {
                format!("{} · {}", part.refdes, net)
            } else {
                format!("{} pin {} · {}", part.refdes, pin.number, net)
            };
            response.clone().on_hover_text(label);
            if response.clicked() {
                self.select_pin(pin_idx);
            }
        } else if response.clicked() {
            self.selected_part = None;
            self.selected_pin = None;
            self.selected_net = None;
        }
    }
}

fn level_color(level: u8) -> Color32 {
    match level {
        0 => Color32::from_rgb(255, 220, 60),
        1 => Color32::from_rgb(255, 140, 40),
        2 => Color32::from_rgb(235, 90, 160),
        _ => Color32::from_rgb(170, 110, 255),
    }
}

fn seg_maybe_visible(a: Pos2, b: Pos2, r: Rect) -> bool {
    let seg = Rect::from_two_pos(a, b);
    r.intersects(seg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fbv_core::{BoardBuilder, BoardFormat, Part, Pin};

    /// PPBUS_G3H --[L7100]-- PPBUS_INT --[R1]-- PP3V3, plus GND which must
    /// never be entered.
    fn jumper_chain() -> Arc<BoardModel> {
        let mut b = BoardBuilder::new();
        let mk_part = |b: &mut BoardBuilder, refdes: &str| {
            b.add_part(Part {
                refdes: refdes.into(),
                mfg_code: None,
                value: None,
                package: None,
                side: Side::Top,
                pins: vec![],
                is_dummy: false,
            })
        };
        let l = mk_part(&mut b, "L7100");
        let r = mk_part(&mut b, "R1");
        let c = mk_part(&mut b, "C9");
        let ppbus = b.net_id("PPBUS_G3H");
        let int = b.net_id("PPBUS_INT");
        let pp3v3 = b.net_id("PP3V3");
        let gnd = b.net_id("GND");
        let pin = |b: &mut BoardBuilder, part, net, x| {
            b.add_pin(Pin {
                part,
                number: String::new(),
                name: String::new(),
                pos: Point::new(x, 0.0),
                radius: 1.0,
                side: Side::Top,
                net,
                is_test_pad: false,
            });
        };
        pin(&mut b, l, ppbus, 0.0);
        pin(&mut b, l, int, 10.0);
        pin(&mut b, r, int, 20.0);
        pin(&mut b, r, pp3v3, 30.0);
        pin(&mut b, c, pp3v3, 40.0);
        pin(&mut b, c, gnd, 50.0);
        Arc::new(b.finish(BoardFormat::Brd))
    }

    #[test]
    fn netweb_origin_prefers_clicked_pin_then_facing_side() {
        let model = jumper_chain();
        let mut tab = BoardTab::new(1, "t".into(), model.clone());
        let ppbus = model.nets.iter().position(|n| n.name == "PPBUS_G3H").unwrap() as NetId;

        // No selection: no origin.
        assert!(tab.netweb_origin().is_none());

        // Net selected without a pin: falls back to a member pin.
        tab.select_net(ppbus);
        let origin = tab.netweb_origin().unwrap();
        assert_eq!(model.pins[origin].net, ppbus);

        // Clicking a specific pin on the net makes it the origin.
        let l7100_pin0 = model.parts[0].pins[0] as usize;
        tab.select_pin(l7100_pin0);
        assert_eq!(tab.netweb_origin(), Some(l7100_pin0));

        // A clicked pin on a DIFFERENT net than the selected one is ignored.
        let gnd_pin = model
            .pins
            .iter()
            .position(|p| model.nets[p.net as usize].name == "GND")
            .unwrap();
        tab.selected_pin = Some(gnd_pin);
        tab.selected_net = Some(ppbus);
        let origin = tab.netweb_origin().unwrap();
        assert_ne!(origin, gnd_pin);
        assert_eq!(model.pins[origin].net, ppbus);
    }

    #[test]
    fn two_pin_parts_get_oriented_outlines() {
        let model = jumper_chain();
        let outlines = compute_part_outlines(&model);
        // L7100: pins at (0,0) and (10,0) — outline must contain both pins
        // with margin, oriented along X.
        let o = outlines[0].expect("L7100 outline");
        assert!((o.center.x - 5.0).abs() < 0.01);
        assert!((o.center.y - 0.0).abs() < 0.01);
        let xs: Vec<f32> = o.corners.iter().map(|c| c.x).collect();
        let ys: Vec<f32> = o.corners.iter().map(|c| c.y).collect();
        let (min_x, max_x) = (xs.iter().cloned().fold(f32::MAX, f32::min), xs.iter().cloned().fold(f32::MIN, f32::max));
        let (min_y, max_y) = (ys.iter().cloned().fold(f32::MAX, f32::min), ys.iter().cloned().fold(f32::MIN, f32::max));
        assert!(min_x < 0.0 && max_x > 10.0, "pins inside outline: {min_x}..{max_x}");
        assert!(min_y < 0.0 && max_y > 0.0);
        // Oriented: longer along the pin axis than across it.
        assert!((max_x - min_x) > (max_y - min_y));
    }

    #[test]
    fn dummy_and_empty_parts_get_no_outline() {
        let mut b = BoardBuilder::new();
        b.add_part(Part {
            refdes: "...".into(),
            mfg_code: None,
            value: None,
            package: None,
            side: Side::Top,
            pins: vec![],
            is_dummy: true,
        });
        let model = b.finish(BoardFormat::Brd);
        let outlines = compute_part_outlines(&model);
        assert!(outlines[0].is_none());
    }

    #[test]
    fn expansion_follows_jumpers_but_not_caps_or_ground() {
        let model = jumper_chain();
        let mut tab = BoardTab::new(1, "t".into(), model.clone());
        let ppbus = model.nets.iter().position(|n| n.name == "PPBUS_G3H").unwrap() as NetId;
        tab.select_net(ppbus);

        tab.expansion = 0;
        assert_eq!(tab.highlight_map().len(), 1);

        tab.expansion = 1;
        let m = tab.highlight_map();
        assert_eq!(m.len(), 2, "level 1 adds PPBUS_INT only: {m:?}");

        tab.expansion = 3;
        let m = tab.highlight_map();
        let names: Vec<&str> = m
            .keys()
            .map(|&n| model.nets[n as usize].name.as_str())
            .collect();
        assert_eq!(m.len(), 3, "{names:?}");
        assert!(!names.contains(&"GND"), "must not expand through C9 or into GND");
        assert_eq!(m[&(model.nets.iter().position(|n| n.name == "PP3V3").unwrap() as NetId)], 2);
    }
}
