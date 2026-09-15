//! Shared vector scene intermediate representation (IR).
//!
//! Both conversion directions (EMF -> SVG and SVG -> EMF) go through this
//! model, which guarantees symmetry of the round trip. The IR deliberately
//! covers only pure vector geometry: text, bitmaps, gradients and clipping
//! regions are out of scope.

mod path;
mod scene;
mod style;
mod transform;

pub use path::{PathSeg, Point};
pub use scene::{Element, Scene};
pub use style::{Fill, FillRule, GradientStop, LineCap, LineJoin, LinearGradient, Rgb, Stroke};
pub use transform::Matrix;
