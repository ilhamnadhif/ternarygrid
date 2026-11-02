//! Spatial data (FR-D5/D7): geometry model, WKT/EWKT and WKB/EWKB parsing,
//! polygon triangulation and a pan/zoom viewer without base-map tiles.
//!
//! Typical use: `SpatialLayer::from_column(name, &rows, geom_col, label_col)`
//! for every geometry column (see [`geometry_columns`]), then
//! [`show_spatial`] with a persistent [`SpatialState`]. Prepared
//! triangulations are cached per layer and rebuilt when the layer's
//! geometries change.

mod geometry;
mod layer;
mod triangulate;
mod view;
mod wkb;
mod wkt;

pub use geometry::{Bounds, Coord, Dims, Geom, Geometry};
pub use layer::{SpatialItem, SpatialLayer, geometry_columns};
pub use triangulate::{MAX_VERTICES, MAX_WORK, Point, Triangulation, signed_area, triangulate};
pub use view::{SpatialResponse, SpatialState, show_spatial};
pub use wkb::{parse_geometry_value, parse_wkb, parse_wkb_hex};
pub use wkt::parse_wkt;
