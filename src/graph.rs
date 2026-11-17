//! Commit graph layout. X is in pixels, Y in rows (row `i` is centred on `i + 0.5`).
//! Port of the lane algorithm used by SourceGit's history view.

use std::collections::VecDeque;

use crate::git::Commit;

pub const LANE_WIDTH: f32 = 14.0;
pub const LANE_COLORS: usize = 8;
const HALF_LANE: f32 = LANE_WIDTH / 2.0;
const HALF_ROW: f32 = 0.5;

pub type Point = (f32, f32);

#[derive(Debug)]
pub struct Lane {
    pub points: Vec<Point>,
    pub color: usize,
}

/// Curve from a merge commit to a lane that already carries its other parent.
#[derive(Debug)]
pub struct Link {
    pub start: Point,
    pub control: Point,
    pub end: Point,
    pub color: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DotKind {
    Normal,
    Head,
    Merge,
}

#[derive(Debug)]
pub struct Dot {
    pub center: Point,
    pub color: usize,
    pub kind: DotKind,
}

#[derive(Debug, Default)]
pub struct Graph {
    pub lanes: Vec<Lane>,
    pub links: Vec<Link>,
    pub dots: Vec<Dot>,
    /// Per commit: where the row text may start.
    pub margins: Vec<f32>,
}

#[derive(Default)]
struct ColorPicker(VecDeque<usize>);

impl ColorPicker {
    fn next(&mut self) -> usize {
        if self.0.is_empty() {
            self.0.extend(0..LANE_COLORS);
        }
        self.0.pop_front().unwrap_or(0)
    }
    fn recycle(&mut self, color: usize) {
        if !self.0.contains(&color) {
            self.0.push_back(color);
        }
    }
}

/// A lane still waiting for the commit `next`.
struct Open {
    lane: usize,
    color: usize,
    next: String,
    last_x: f32,
    last_y: f32,
    end_y: f32,
}

impl Open {
    fn new(g: &mut Graph, next: &str, color: usize, points: &[Point]) -> Self {
        let (last_x, last_y) = points[points.len() - 1];
        g.lanes.push(Lane { points: points.to_vec(), color });
        Self { lane: g.lanes.len() - 1, color, next: next.into(), last_x, last_y, end_y: 0.0 }
    }

    fn add(&mut self, g: &mut Graph, x: f32, y: f32) {
        if self.end_y < y {
            g.lanes[self.lane].points.push((x, y));
            self.end_y = y;
        }
    }

    /// The lane passes this row without a commit.
    fn pass(&mut self, g: &mut Graph, x: f32, mut y: f32) {
        if x > self.last_x {
            self.add(g, self.last_x, self.last_y);
            self.add(g, x, y - HALF_ROW);
        } else if x < self.last_x {
            self.add(g, self.last_x, y - HALF_ROW);
            y += HALF_ROW;
            self.add(g, x, y);
        }
        self.last_x = x;
        self.last_y = y;
    }

    /// The lane has its commit in this row and continues.
    fn goto(&mut self, g: &mut Graph, x: f32, y: f32) {
        if x > self.last_x {
            self.add(g, self.last_x, self.last_y);
            self.add(g, x, y - HALF_ROW);
        } else if x < self.last_x {
            let mut min_y = y - HALF_ROW;
            if min_y > self.last_y {
                min_y -= HALF_ROW;
            }
            self.add(g, self.last_x, min_y);
            self.add(g, x, y);
        }
        self.last_x = x;
        self.last_y = y;
    }

    /// The lane ends in this row.
    fn end(&mut self, g: &mut Graph, x: f32, y: f32) {
        if x > self.last_x {
            self.add(g, self.last_x, self.last_y);
            self.add(g, x, y - HALF_ROW);
        } else if x < self.last_x {
            self.add(g, self.last_x, y - HALF_ROW);
        }
        self.add(g, x, y);
        self.last_x = x;
        self.last_y = y;
    }
}

pub fn layout(commits: &[Commit]) -> Graph {
    let mut g = Graph::default();
    let mut open: Vec<Open> = Vec::new();
    let mut colors = ColorPicker::default();
    let mut y = -HALF_ROW;

    for commit in commits {
        y += 1.0;
        let mut x = 4.0 - HALF_LANE;
        let max_old = open.last().map_or(x + LANE_WIDTH, |l| l.last_x);

        // The first lane waiting for this commit carries on; the others end here.
        let mut major: Option<(f32, usize)> = None;
        let mut ended = Vec::new();
        for (i, lane) in open.iter_mut().enumerate() {
            if lane.next != commit.sha {
                x += LANE_WIDTH;
                lane.pass(&mut g, x, y);
                continue;
            }
            if let Some((major_x, _)) = major {
                lane.end(&mut g, major_x, y);
                ended.push(i);
                continue;
            }
            x += LANE_WIDTH;
            match commit.parents.first() {
                Some(parent) => {
                    lane.next = parent.clone();
                    lane.goto(&mut g, x, y);
                }
                None => {
                    lane.end(&mut g, x, y);
                    ended.push(i);
                }
            }
            major = Some((lane.last_x, lane.color));
        }
        for i in ended.into_iter().rev() {
            colors.recycle(open.remove(i).color);
        }

        // No lane was waiting: this commit is a branch tip and starts a lane.
        if major.is_none() {
            x += LANE_WIDTH;
            if let Some(parent) = commit.parents.first() {
                let lane = Open::new(&mut g, parent, colors.next(), &[(x, y)]);
                major = Some((lane.last_x, lane.color));
                open.push(lane);
            }
        }

        let (dot_x, color) = major.unwrap_or((x, 0));
        let kind = if commit.is_head {
            DotKind::Head
        } else if commit.parents.len() > 1 {
            DotKind::Merge
        } else {
            DotKind::Normal
        };
        g.dots.push(Dot { center: (dot_x, y), color, kind });

        for parent in commit.parents.iter().skip(1) {
            if let Some(lane) = open.iter().find(|l| &l.next == parent) {
                g.links.push(Link {
                    start: (dot_x, y),
                    control: (lane.last_x, y),
                    end: (lane.last_x, y + HALF_ROW),
                    color: lane.color,
                });
            } else {
                x += LANE_WIDTH;
                open.push(Open::new(&mut g, parent, colors.next(), &[(dot_x, y), (x, y + HALF_ROW)]));
            }
        }

        g.margins.push(x.max(max_old) + HALF_LANE + 2.0);
    }

    // Lanes whose parent lies beyond the loaded history run off the bottom.
    let end_y = commits.len() as f32 - 0.5;
    for (i, lane) in open.iter_mut().enumerate() {
        let points = &g.lanes[lane.lane].points;
        if points.len() == 1 && (points[0].1 - end_y).abs() < 1e-4 {
            continue;
        }
        lane.end(&mut g, (i as f32 + 0.5) * LANE_WIDTH + 4.0, end_y + HALF_ROW);
    }
    g
}
